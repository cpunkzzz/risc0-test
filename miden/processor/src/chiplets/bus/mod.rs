use super::{
    hasher::HasherLookup, BTreeMap, BitwiseLookup, Felt, FieldElement, LookupTableRow,
    MemoryLookup, Vec,
};

mod aux_trace;
pub use aux_trace::AuxTraceBuilder;

// CHIPLETS BUS
// ================================================================================================

/// The Chiplets bus tracks data requested from or provided by chiplets in the Chiplets module. It
/// processes lookup requests from the stack & decoder and response data from the chiplets.
///
/// For correct execution, the lookup data used by the stack for each chiplet must be a permutation
/// of the lookups executed by that chiplet so that they cancel out. This is ensured by the `b_chip`
/// bus column. When the `b_chip` column is built, requests from the stack must be divided out and
/// lookup results provided by the chiplets must be multiplied in. To ensure that all lookups are
/// attributed to the correct chiplet and operation, a unique chiplet operation label must be
/// included in the lookup row value when it is computed.

#[derive(Default)]
pub struct ChipletsBus {
    lookup_hints: BTreeMap<usize, ChipletsLookup>,
    request_rows: Vec<ChipletsLookupRow>,
    response_rows: Vec<ChipletsLookupRow>,
    // TODO: remove queued requests by refactoring the hasher/decoder interactions so that the
    // lookups are built as they are requested. This will be made easier by removing state info from
    // the HasherLookup struct. Primarily it will require a refactor of `hash_span_block`,
    // `start_span_block`, `respan`, and `end_span_block`.
    queued_requests: Vec<HasherLookup>,
}

impl ChipletsBus {
    // LOOKUP MUTATORS
    // --------------------------------------------------------------------------------------------

    /// Requests lookups for a single operation at the specified cycle. A Hasher operation request
    /// can contain one or more lookups, while Bitwise and Memory requests will only contain a
    /// single lookup.
    fn request_lookup(&mut self, cycle: usize) {
        // all requests are sent from the stack before responses are provided (during Chiplets trace
        // finalization). requests are guaranteed not to share cycles with other requests, since
        // only one operation will be executed at a time.
        let request_idx = self.request_rows.len();
        self.lookup_hints
            .insert(cycle, ChipletsLookup::Request(request_idx));
    }

    /// Provides lookup data at the specified cycle, which is the row of the Chiplets execution
    /// trace that contains this lookup row.
    fn provide_lookup(&mut self, response_cycle: usize) {
        // results are guaranteed not to share cycles with other results, but they might share
        // a cycle with a request which has already been sent.
        let response_idx = self.response_rows.len();
        self.lookup_hints
            .entry(response_cycle)
            .and_modify(|lookup| {
                if let ChipletsLookup::Request(request_idx) = *lookup {
                    *lookup = ChipletsLookup::RequestAndResponse((request_idx, response_idx));
                }
            })
            .or_insert_with(|| ChipletsLookup::Response(response_idx));
    }

    // HASHER LOOKUPS
    // --------------------------------------------------------------------------------------------

    /// Requests lookups at the specified `cycle` for the initial row and result row of Hash
    /// operations in the Hash Chiplet. This request is expected to originate from operation
    /// executors requesting one or more hash operations for the Stack where all operation lookups
    /// must be included at the same cycle. For simple permutations this will require 2 lookups,
    /// while for a Merkle root update it will require 4, since two Hash operations are required.
    pub fn request_hasher_operation(&mut self, lookups: &[HasherLookup], cycle: usize) {
        debug_assert!(
            lookups.len() == 2 || lookups.len() == 4,
            "incorrect number of lookup rows for hasher operation request"
        );
        self.request_lookup(cycle);
        self.request_rows
            .push(ChipletsLookupRow::HasherMulti(lookups.to_vec()));
    }

    /// Requests the specified lookup from the Hash Chiplet at the specified `cycle`. Single lookup
    /// requests are expected to originate from the decoder during control block decoding. This
    /// lookup can be for either the initial or the final row of the hash operation.
    pub fn request_hasher_lookup(&mut self, lookup: HasherLookup, cycle: usize) {
        self.request_lookup(cycle);
        self.request_rows.push(ChipletsLookupRow::Hasher(lookup));
    }

    /// Adds the request for the specified lookup to a queue from which it can be sent later when
    /// the cycle of the request is known. Queued requests are expected to originate from the
    /// decoder, since the hash is computed at the start of each control block (along with all
    /// required lookups), but the decoder does not request intermediate and final lookups until the
    /// end of the control block or until a `RESPAN`, in the case of `SPAN` blocks with more than
    /// one operation batch.
    pub fn enqueue_hasher_request(&mut self, lookup: HasherLookup) {
        self.queued_requests.push(lookup);
    }

    /// Pops the top HasherLookup request off the queue and sends it to the bus. This request is
    /// expected to originate from the decoder as it continues or finalizes control blocks with
    /// `RESPAN` or `END`.
    pub fn send_queued_hasher_request(&mut self, cycle: usize) {
        let lookup = self.queued_requests.pop();
        debug_assert!(lookup.is_some(), "no queued requests");

        if let Some(lookup) = lookup {
            self.request_hasher_lookup(lookup, cycle);
        }
    }

    /// Provides the data of a hash chiplet operation contained in the [Hasher] table. The hash
    /// lookup value is provided at cycle `response_cycle`, which is the row of the execution trace
    /// that contains this Hasher row. It will always be either the first or last row of a Hasher
    /// operation cycle.
    pub fn provide_hasher_lookup(&mut self, lookup: HasherLookup, response_cycle: usize) {
        self.provide_lookup(response_cycle);
        self.response_rows.push(ChipletsLookupRow::Hasher(lookup));
    }

    // BITWISE LOOKUPS
    // --------------------------------------------------------------------------------------------

    /// Requests the specified bitwise lookup at the specified `cycle`. This request is expected to
    /// originate from operation executors.
    pub fn request_bitwise_operation(&mut self, lookup: BitwiseLookup, cycle: usize) {
        self.request_lookup(cycle);
        self.request_rows.push(ChipletsLookupRow::Bitwise(lookup));
    }

    /// Provides the data of a bitwise operation contained in the [Bitwise] table. The bitwise value
    /// is provided at cycle `response_cycle`, which is the row of the execution trace that contains
    /// this Bitwise row. It will always be the final row of a Bitwise operation cycle.
    pub fn provide_bitwise_operation(&mut self, lookup: BitwiseLookup, response_cycle: usize) {
        self.provide_lookup(response_cycle);
        self.response_rows.push(ChipletsLookupRow::Bitwise(lookup));
    }

    // MEMORY LOOKUPS
    // --------------------------------------------------------------------------------------------

    /// Sends a request for the specified memory access. When `old_word` and `new_word` are the
    /// same in the MemoryLookup, this is a read request. When they are different, it's a write
    /// request. The memory value is requested at `cycle`. This request is expected to originate
    /// from operation executors.
    pub fn request_memory_operation(&mut self, lookup: MemoryLookup, cycle: usize) {
        self.request_lookup(cycle);
        self.request_rows.push(ChipletsLookupRow::Memory(lookup));
    }

    /// Provides the data of the specified memory access.  When `old_word` and `new_word` are the
    /// same in the MemoryLookup, this is a read request. When they are different, it's a write  
    /// request. The memory access data is provided at cycle `response_cycle`, which is the row of
    /// the execution trace that contains this Memory row.
    pub fn provide_memory_operation(&mut self, lookup: MemoryLookup, response_cycle: usize) {
        self.provide_lookup(response_cycle);
        self.response_rows.push(ChipletsLookupRow::Memory(lookup));
    }

    // AUX TRACE BUILDER GENERATION
    // --------------------------------------------------------------------------------------------

    /// Converts this [ChipletsBus] into an auxiliary trace builder which can be used to construct
    /// the auxiliary trace column describing the [Chiplets] lookups at every cycle.
    pub fn into_aux_builder(self) -> AuxTraceBuilder {
        let lookup_hints = self.lookup_hints.into_iter().collect();

        AuxTraceBuilder {
            lookup_hints,
            request_rows: self.request_rows,
            response_rows: self.response_rows,
        }
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns an option with the lookup hint for the specified cycle.
    #[cfg(test)]
    pub(super) fn get_lookup_hint(&self, cycle: usize) -> Option<&ChipletsLookup> {
        self.lookup_hints.get(&cycle)
    }

    /// Returns the ith lookup response provided by the Chiplets module.
    #[cfg(test)]
    pub(super) fn get_response_row(&self, i: usize) -> ChipletsLookupRow {
        self.response_rows[i].clone()
    }
}

// CHIPLETS LOOKUPS
// ================================================================================================

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum ChipletsLookup {
    Request(usize),
    Response(usize),
    RequestAndResponse((usize, usize)),
}

// TODO: investigate alternative approaches, since this is heavy (e.g. read from execution trace)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChipletsLookupRow {
    Hasher(HasherLookup),
    HasherMulti(Vec<HasherLookup>),
    Bitwise(BitwiseLookup),
    Memory(MemoryLookup),
}

impl LookupTableRow for ChipletsLookupRow {
    fn to_value<E: FieldElement<BaseField = Felt>>(&self, alphas: &[E]) -> E {
        match self {
            ChipletsLookupRow::HasherMulti(lookups) => lookups
                .iter()
                .fold(E::ONE, |acc, row| acc * row.to_value(alphas)),
            ChipletsLookupRow::Hasher(row) => row.to_value(alphas),
            ChipletsLookupRow::Bitwise(row) => row.to_value(alphas),
            ChipletsLookupRow::Memory(row) => row.to_value(alphas),
        }
    }
}
