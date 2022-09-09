#![no_main]
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use anyhow::{anyhow, Result};
use miden_air::FieldElement;
use risc0_zkvm_guest::{env, sha};
use rkyv::{option::ArchivedOption, Archive, Deserialize};
use utils::fib::fib_air::FibAir;
use utils::inputs::{FibAirInput, FibRiscInput, Output};
use winter_air::{Air, AuxTraceRandElements, ConstraintCompositionCoefficients, EvaluationFrame};
use winter_crypto::ElementHasher;
use winter_crypto::{
    hashers::{Sha2_256, ShaHasherT},
    ByteDigest, RandomCoin,
};
use winter_math::fields::f64::BaseElement;
use winter_verifier::evaluate_constraints;

risc0_zkvm_guest::entry!(main);

pub struct GuestSha2;

impl ShaHasherT for GuestSha2 {
    fn digest(data: &[u8]) -> [u8; 32] {
        sha::digest_u8_slice(data).get_u8()
    }
}

type E = BaseElement;
type H = Sha2_256<E, GuestSha2>;

pub fn aux_trace_segments(
    risc_input: &<FibRiscInput<E> as Archive>::Archived,
    public_coin: &mut RandomCoin<E, Sha2_256<E, GuestSha2>>,
    air: &FibAir,
) -> Result<AuxTraceRandElements<E>> {
    let first_digest = ByteDigest::new(risc_input.trace_commitments[0]);
    public_coin.reseed(first_digest);
    let mut aux_trace_rand_elements = AuxTraceRandElements::<E>::new();
    for (i, commitment) in risc_input.trace_commitments.iter().skip(1).enumerate() {
        let rand_elements = air
            .get_aux_trace_segment_random_elements(i, public_coin)
            .map_err(|_| anyhow!("Random coin error"))?;
        aux_trace_rand_elements.add_segment_elements(rand_elements);
        let c = ByteDigest::new(*commitment);
        public_coin.reseed(c);
    }
    Ok(aux_trace_rand_elements)
}

pub fn get_constraint_coffs(
    public_coin: &mut RandomCoin<E, Sha2_256<E, GuestSha2>>,
    air: &FibAir,
) -> Result<ConstraintCompositionCoefficients<E>> {
    let constraint_coeffs = air
        .get_constraint_composition_coefficients(public_coin)
        .map_err(|_| anyhow!("Random coin error"))?;
    Ok(constraint_coeffs)
}

pub fn main() {
    let aux_input: &[u8] = env::read_aux_input();
    let air_input: FibAirInput = env::read();
    let risc_input = unsafe { rkyv::archived_root::<FibRiscInput<E>>(&aux_input[..]) };
    let air = FibAir::new(
        air_input.trace_info,
        risc_input
            .result
            .deserialize(&mut rkyv::Infallible)
            .unwrap(),
        air_input.proof_options,
    );

    let public_coin_seed = Vec::new();
    let mut public_coin: RandomCoin<E, Sha2_256<E, GuestSha2>> = RandomCoin::new(&public_coin_seed);
    // process auxiliary trace segments (if any), to build a set of random elements for each segment
    let aux_trace_rand_elements =
        aux_trace_segments(&risc_input, &mut public_coin, &air).expect("aux trace segments failed");
    // build random coefficients for the composition polynomial
    let constraint_coeffs =
        get_constraint_coffs(&mut public_coin, &air).expect("constraint_coeffs_error");
    env::log(&format!("constraint coeffs: {:?}", &constraint_coeffs));
    let constraint_commitment = ByteDigest::new(risc_input.constraint_commitment);
    public_coin.reseed(constraint_commitment);
    let z = public_coin
        .draw::<E>()
        .map_err(|_| anyhow!("Random coin error"))
        .expect("constraint_commitment");

    // 3 ----- OOD consistency check --------------------------------------------------------------
    // make sure that evaluations obtained by evaluating constraints over the out-of-domain frame
    // are consistent with the evaluations of composition polynomial columns sent by the prover

    // read the out-of-domain trace frames (the main trace frame and auxiliary trace frame, if
    // provided) sent by the prover and evaluate constraints over them; also, reseed the public
    // coin with the OOD frames received from the prover.
    let ood_main_trace_frame: EvaluationFrame<E> = EvaluationFrame::from_rows(
        risc_input
            .ood_main_trace_frame
            .current
            .deserialize(&mut rkyv::Infallible)
            .unwrap(),
        risc_input
            .ood_main_trace_frame
            .next
            .deserialize(&mut rkyv::Infallible)
            .unwrap(),
    );

    let ood_aux_trace_frame: Option<EvaluationFrame<E>> = match &risc_input.ood_aux_trace_frame {
        ArchivedOption::None => None,
        ArchivedOption::Some(row) => Some(EvaluationFrame::from_rows(
            row.current.deserialize(&mut rkyv::Infallible).unwrap(),
            row.next.deserialize(&mut rkyv::Infallible).unwrap(),
        )),
    };

    let ood_constraint_evaluation_1 = evaluate_constraints(
        &air,
        constraint_coeffs,
        &ood_main_trace_frame,
        &ood_aux_trace_frame,
        aux_trace_rand_elements,
        z,
    );

    if let Some(ref aux_trace_frame) = ood_aux_trace_frame {
        // when the trace contains auxiliary segments, append auxiliary trace elements at the
        // end of main trace elements for both current and next rows in the frame. this is
        // needed to be consistent with how the prover writes OOD frame into the channel.

        let mut current = ood_main_trace_frame.current().to_vec();
        current.extend_from_slice(aux_trace_frame.current());
        public_coin.reseed(H::hash_elements(&current));

        let mut next = ood_main_trace_frame.next().to_vec();
        next.extend_from_slice(aux_trace_frame.next());
        public_coin.reseed(H::hash_elements(&next));
    } else {
        public_coin.reseed(H::hash_elements(ood_main_trace_frame.current()));
        public_coin.reseed(H::hash_elements(ood_main_trace_frame.next()));
    }

    // read evaluations of composition polynomial columns sent by the prover, and reduce them into
    // a single value by computing sum(z^i * value_i), where value_i is the evaluation of the ith
    // column polynomial at z^m, where m is the total number of column polynomials; also, reseed
    // the public coin with the OOD constraint evaluations received from the prover.
    let ood_constraint_evaluations: Vec<E> = risc_input
        .ood_constraint_evaluations
        .deserialize(&mut rkyv::Infallible)
        .unwrap();
    let ood_constraint_evaluation_2 = ood_constraint_evaluations
        .iter()
        .enumerate()
        .fold(E::ZERO, |result, (i, &value)| {
            result + z.exp((i as u32).into()) * value
        });
    public_coin.reseed(H::hash_elements(&ood_constraint_evaluations));

    // finally, make sure the values are the same
    // if ood_constraint_evaluation_1 != ood_constraint_evaluation_2 {
    //     panic!("Inconsistent OOD constraint evaluations");
    // }
    env::commit(&Output::<E> {
        ood_constraint_evaluation_1,
        ood_constraint_evaluation_2,
    });
}
