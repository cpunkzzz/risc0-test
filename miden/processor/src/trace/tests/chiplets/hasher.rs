use core::ops::Range;

use super::{
    build_span_with_respan_ops, build_trace_from_block, build_trace_from_ops_with_inputs,
    rand_array, ExecutionTrace, Felt, FieldElement, Operation, Trace, AUX_TRACE_RAND_ELEMENTS,
    CHIPLETS_AUX_TRACE_OFFSET, NUM_RAND_ROWS, ONE, ZERO,
};

use vm_core::{
    chiplets::{
        hasher::{
            apply_permutation, init_state_from_words, HasherState, Selectors, CAPACITY_LEN,
            DIGEST_RANGE, HASH_CYCLE_LEN, LINEAR_HASH, LINEAR_HASH_LABEL, MP_VERIFY,
            MP_VERIFY_LABEL, MR_UPDATE_NEW, MR_UPDATE_NEW_LABEL, MR_UPDATE_OLD,
            MR_UPDATE_OLD_LABEL, RETURN_HASH, RETURN_HASH_LABEL, RETURN_STATE, RETURN_STATE_LABEL,
            STATE_WIDTH,
        },
        HASHER_NODE_INDEX_COL_IDX, HASHER_ROW_COL_IDX, HASHER_STATE_COL_RANGE, HASHER_TRACE_OFFSET,
    },
    code_blocks::CodeBlock,
    utils::range,
    AdviceSet, ProgramInputs, StarkField, Word, DECODER_TRACE_OFFSET,
};

// CONSTANTS
// ================================================================================================

const DECODER_HASHER_STATE_RANGE: Range<usize> = range(
    DECODER_TRACE_OFFSET + vm_core::decoder::HASHER_STATE_OFFSET,
    vm_core::decoder::NUM_HASHER_COLUMNS,
);

// TESTS
// ================================================================================================

/// Tests the generation of the `b_chip` bus column when the hasher only performs a single `SPAN`
/// with one operation batch.
#[test]
#[allow(clippy::needless_range_loop)]
pub fn b_chip_span() {
    let program = CodeBlock::new_span(vec![Operation::Add, Operation::Mul]);
    let mut trace = build_trace_from_block(&program, &[]);

    let alphas = rand_array::<Felt, AUX_TRACE_RAND_ELEMENTS>();
    let aux_columns = trace.build_aux_segment(&[], &alphas).unwrap();
    let b_chip = aux_columns.get_column(CHIPLETS_AUX_TRACE_OFFSET);

    assert_eq!(trace.length(), b_chip.len());
    assert_eq!(ONE, b_chip[0]);

    // at the first cycle the following are added for inclusion in the next row:
    // - the initialization of the span hash is requested by the decoder
    // - the initialization of the span hash is provided by the hasher

    // initialize the request state with capacity equal to the number of operation groups.
    let mut state = [ZERO; STATE_WIDTH];
    state[0] = ONE;
    fill_state_from_decoder(&trace, &mut state, 0);
    // request the initialization of the span hash
    let request_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        state,
        [ZERO; STATE_WIDTH],
        ONE,
        ZERO,
    );
    let mut expected = request_init.inv();

    // provide the initialization of the span hash
    expected *= build_expected_from_trace(&trace, &alphas, 0);
    assert_eq!(expected, b_chip[1]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 2..4 {
        assert_eq!(expected, b_chip[row]);
    }

    // At cycle 3 the decoder requests the result of the span hash.
    apply_permutation(&mut state);
    let request_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        state,
        [ZERO; STATE_WIDTH],
        Felt::new(HASH_CYCLE_LEN as u64),
        ZERO,
    );
    expected *= request_result.inv();
    assert_eq!(expected, b_chip[4]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 5..HASH_CYCLE_LEN {
        assert_eq!(expected, b_chip[row]);
    }

    // At the end of the hash cycle, the result of the span hash is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, HASH_CYCLE_LEN - 1);
    assert_eq!(expected, b_chip[HASH_CYCLE_LEN]);

    // The value in b_chip should be ONE now and for the rest of the trace.
    for row in HASH_CYCLE_LEN..trace.length() - NUM_RAND_ROWS {
        assert_eq!(ONE, b_chip[row]);
    }
}

/// Tests the generation of the `b_chip` bus column when the hasher only performs a `SPAN` but it
/// includes multiple batches.
#[test]
#[allow(clippy::needless_range_loop)]
pub fn b_chip_span_with_respan() {
    let (ops, _) = build_span_with_respan_ops();
    let program = CodeBlock::new_span(ops);
    let mut trace = build_trace_from_block(&program, &[]);

    let alphas = rand_array::<Felt, AUX_TRACE_RAND_ELEMENTS>();
    let aux_columns = trace.build_aux_segment(&[], &alphas).unwrap();
    let b_chip = aux_columns.get_column(CHIPLETS_AUX_TRACE_OFFSET);

    assert_eq!(trace.length(), b_chip.len());
    assert_eq!(ONE, b_chip[0]);

    // at cycle 0 the following are added for inclusion in the next row:
    // - the initialization of the span hash is requested by the decoder
    // - the initialization of the span hash is provided by the hasher

    // initialize the request state with capacity equal to the number of operation groups.
    let mut state = [ZERO; STATE_WIDTH];
    state[0] = Felt::new(12);
    fill_state_from_decoder(&trace, &mut state, 0);
    // request the initialization of the span hash
    let request_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        state,
        [ZERO; STATE_WIDTH],
        ONE,
        ZERO,
    );
    let mut expected = request_init.inv();

    // provide the initialization of the span hash
    expected *= build_expected_from_trace(&trace, &alphas, 0);
    assert_eq!(expected, b_chip[1]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 2..8 {
        assert_eq!(expected, b_chip[row]);
    }

    // At the end of the first hash cycle at cycle 7, the absorption of the next operation batch is
    // provided by the hasher.
    expected *= build_expected_from_trace(&trace, &alphas, 7);
    assert_eq!(expected, b_chip[8]);

    // Nothing changes when there is no communication with the hash chiplet.
    assert_eq!(expected, b_chip[9]);

    // At cycle 9, after the first operation batch, the decoder initiates a respan and requests the
    // absorption of the next operation batch.
    apply_permutation(&mut state);
    let prev_state = state;
    // get the state with the next absorbed batch.
    absorb_state_from_decoder(&trace, &mut state, 9);
    let request_respan = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        prev_state,
        state,
        Felt::new(8),
        ZERO,
    );
    expected *= request_respan.inv();
    assert_eq!(expected, b_chip[10]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 11..16 {
        assert_eq!(expected, b_chip[row]);
    }

    // At cycle 15 at the end of the second hash cycle, the result of the span hash is provided by
    // the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 15);
    assert_eq!(expected, b_chip[16]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 17..22 {
        assert_eq!(expected, b_chip[row]);
    }

    // At cycle 21, after the second operation batch, the decoder ends the SPAN block and requests
    // its hash.
    apply_permutation(&mut state);
    let request_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        state,
        [ZERO; STATE_WIDTH],
        Felt::new(16),
        ZERO,
    );
    expected *= request_result.inv();
    assert_eq!(expected, b_chip[22]);

    // The value in b_chip should be ONE now and for the rest of the trace.
    for row in 22..trace.length() - NUM_RAND_ROWS {
        assert_eq!(ONE, b_chip[row]);
    }
}

/// Tests the generation of the `b_chip` bus column when the hasher performs a merge of two code
/// blocks requested by the decoder. (This also requires a `SPAN` block.)
#[test]
#[allow(clippy::needless_range_loop)]
pub fn b_chip_merge() {
    let t_branch = CodeBlock::new_span(vec![Operation::Add]);
    let f_branch = CodeBlock::new_span(vec![Operation::Mul]);
    let program = CodeBlock::new_split(t_branch, f_branch);
    let mut trace = build_trace_from_block(&program, &[]);

    let alphas = rand_array::<Felt, AUX_TRACE_RAND_ELEMENTS>();
    let aux_columns = trace.build_aux_segment(&[], &alphas).unwrap();
    let b_chip = aux_columns.get_column(CHIPLETS_AUX_TRACE_OFFSET);

    assert_eq!(trace.length(), b_chip.len());
    assert_eq!(ONE, b_chip[0]);

    // at cycle 0 the following are added for inclusion in the next row:
    // - the initialization of the merge of the split's child hashes is requested by the decoder
    // - the initialization of the code block merge is provided by the hasher

    // initialize the request state with capacity equal to the number of operation groups.
    let mut split_state = [ZERO; STATE_WIDTH];
    split_state[0] = Felt::new(8);
    fill_state_from_decoder(&trace, &mut split_state, 0);
    // request the initialization of the span hash
    let split_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        split_state,
        [ZERO; STATE_WIDTH],
        ONE,
        ZERO,
    );
    let mut expected = split_init.inv();

    // provide the initialization of the span hash
    expected *= build_expected_from_trace(&trace, &alphas, 0);
    assert_eq!(expected, b_chip[1]);

    // at cycle 1 the initialization of the span block hash for the false branch is requested by the
    // decoder
    let mut f_branch_state = [ZERO; STATE_WIDTH];
    f_branch_state[0] = Felt::new(1);
    fill_state_from_decoder(&trace, &mut f_branch_state, 1);
    // request the initialization of the false branch hash
    let f_branch_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        f_branch_state,
        [ZERO; STATE_WIDTH],
        Felt::new(9),
        ZERO,
    );
    expected *= f_branch_init.inv();
    assert_eq!(expected, b_chip[2]);

    // Nothing changes when there is no communication with the hash chiplet.
    assert_eq!(expected, b_chip[3]);

    // at cycle 3 the result hash of the span block for the false branch is requested by the decoder
    apply_permutation(&mut f_branch_state);
    let f_branch_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        f_branch_state,
        [ZERO; STATE_WIDTH],
        Felt::new(16),
        ZERO,
    );
    expected *= f_branch_result.inv();
    assert_eq!(expected, b_chip[4]);

    // at cycle 4 the result of the split code block's hash is requested by the decoder
    apply_permutation(&mut split_state);
    let split_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        split_state,
        [ZERO; STATE_WIDTH],
        Felt::new(8),
        ZERO,
    );
    expected *= split_result.inv();
    assert_eq!(expected, b_chip[5]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 6..8 {
        assert_eq!(expected, b_chip[row]);
    }

    // at cycle 7 the result of the merge is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 7);
    assert_eq!(expected, b_chip[8]);

    // at cycle 8 the initialization of the hash of the span block for the false branch is provided
    // by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 8);
    assert_eq!(expected, b_chip[9]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 9..16 {
        assert_eq!(expected, b_chip[row]);
    }

    // at cycle 15 the result of the span block for the false branch is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 15);
    assert_eq!(expected, b_chip[16]);

    // The value in b_chip should be ONE now and for the rest of the trace.
    for row in 16..trace.length() - NUM_RAND_ROWS {
        assert_eq!(ONE, b_chip[row]);
    }
}

/// Tests the generation of the `b_chip` bus column when the hasher performs a permutation
/// requested by the `RpPerm` user operation.
#[test]
#[allow(clippy::needless_range_loop)]
pub fn b_chip_permutation() {
    let program = CodeBlock::new_span(vec![Operation::RpPerm]);
    let stack = vec![8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
    let mut trace = build_trace_from_block(&program, &stack);

    let mut rpperm_state: [Felt; STATE_WIDTH] = stack
        .iter()
        .map(|v| Felt::new(*v))
        .collect::<Vec<_>>()
        .try_into()
        .expect("failed to convert vector to array");
    let alphas = rand_array::<Felt, AUX_TRACE_RAND_ELEMENTS>();
    let aux_columns = trace.build_aux_segment(&[], &alphas).unwrap();
    let b_chip = aux_columns.get_column(CHIPLETS_AUX_TRACE_OFFSET);

    assert_eq!(trace.length(), b_chip.len());
    assert_eq!(ONE, b_chip[0]);

    // at cycle 0 the following are added for inclusion in the next row:
    // - the initialization of the span hash is requested by the decoder
    // - the initialization of the span hash is provided by the hasher

    // initialize the request state with capacity equal to the number of operation groups.
    let mut span_state = [ZERO; STATE_WIDTH];
    span_state[0] = ONE;
    fill_state_from_decoder(&trace, &mut span_state, 0);
    // request the initialization of the span hash
    let span_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        span_state,
        [ZERO; STATE_WIDTH],
        ONE,
        ZERO,
    );
    let mut expected = span_init.inv();
    // provide the initialization of the span hash
    expected *= build_expected_from_trace(&trace, &alphas, 0);
    assert_eq!(expected, b_chip[1]);

    // at cycle 1 rpperm is executed and the initialization and result of the hash are both
    // requested by the stack.
    let rpperm_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        rpperm_state,
        [ZERO; STATE_WIDTH],
        Felt::new(9),
        ZERO,
    );
    // request the rpperm initialization.
    expected *= rpperm_init.inv();
    apply_permutation(&mut rpperm_state);
    let rpperm_result = build_expected(
        &alphas,
        RETURN_STATE_LABEL,
        rpperm_state,
        [ZERO; STATE_WIDTH],
        Felt::new(16),
        ZERO,
    );
    // request the rpperm result.
    expected *= rpperm_result.inv();
    assert_eq!(expected, b_chip[2]);

    // at cycle 2 the result of the span hash is requested by the decoder
    apply_permutation(&mut span_state);
    let span_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        span_state,
        [ZERO; STATE_WIDTH],
        Felt::new(8),
        ZERO,
    );
    expected *= span_result.inv();
    assert_eq!(expected, b_chip[3]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 4..8 {
        assert_eq!(expected, b_chip[row]);
    }

    // at cycle 7 the result of the span hash is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 7);
    assert_eq!(expected, b_chip[8]);

    // at cycle 8 the initialization of the rpperm hash is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 8);
    assert_eq!(expected, b_chip[9]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 10..16 {
        assert_eq!(expected, b_chip[row]);
    }

    // at cycle 15 the result of the rpperm hash is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 15);
    assert_eq!(expected, b_chip[16]);

    // The value in b_chip should be ONE now and for the rest of the trace.
    for row in 16..trace.length() - NUM_RAND_ROWS {
        assert_eq!(ONE, b_chip[row]);
    }
}

/// Tests the generation of the `b_chip` bus column when the hasher performs a Merkle path
/// verification requested by the `MpVerify` user operation.
#[test]
#[allow(clippy::needless_range_loop)]
fn b_chip_mpverify() {
    let index = 5usize;
    let leaves = init_leaves(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let tree = AdviceSet::new_merkle_tree(leaves.to_vec()).unwrap();

    let stack_inputs = [
        tree.root()[0].as_int(),
        tree.root()[1].as_int(),
        tree.root()[2].as_int(),
        tree.root()[3].as_int(),
        leaves[index][0].as_int(),
        leaves[index][1].as_int(),
        leaves[index][2].as_int(),
        leaves[index][3].as_int(),
        index as u64,
        tree.depth() as u64,
    ];
    let inputs = ProgramInputs::new(&stack_inputs, &[], vec![tree.clone()]).unwrap();

    let mut trace = build_trace_from_ops_with_inputs(vec![Operation::MpVerify], inputs);
    let alphas = rand_array::<Felt, AUX_TRACE_RAND_ELEMENTS>();
    let aux_columns = trace.build_aux_segment(&[], &alphas).unwrap();
    let b_chip = aux_columns.get_column(CHIPLETS_AUX_TRACE_OFFSET);

    assert_eq!(trace.length(), b_chip.len());
    assert_eq!(ONE, b_chip[0]);

    // at cycle 0 the following are added for inclusion in the next row:
    // - the initialization of the span hash is requested by the decoder
    // - the initialization of the span hash is provided by the hasher

    // initialize the request state with capacity equal to the number of operation groups.
    let mut span_state = [ZERO; STATE_WIDTH];
    span_state[0] = ONE;
    fill_state_from_decoder(&trace, &mut span_state, 0);
    // request the initialization of the span hash
    let span_init = build_expected(
        &alphas,
        LINEAR_HASH_LABEL,
        span_state,
        [ZERO; STATE_WIDTH],
        ONE,
        ZERO,
    );
    let mut expected = span_init.inv();
    // provide the initialization of the span hash
    expected *= build_expected_from_trace(&trace, &alphas, 0);
    assert_eq!(expected, b_chip[1]);

    // at cycle 1 a merkle path verification is executed and the initialization and result of the
    // hash are both requested by the stack.
    let path = tree
        .get_path(tree.depth(), index as u64)
        .expect("failed to get Merkle tree path");
    let mp_state = init_state_from_words(
        &[path[0][0], path[0][1], path[0][2], path[0][3]],
        &[
            leaves[index][0],
            leaves[index][1],
            leaves[index][2],
            leaves[index][3],
        ],
    );
    let mp_init = build_expected(
        &alphas,
        MP_VERIFY_LABEL,
        mp_state,
        [ZERO; STATE_WIDTH],
        Felt::new(9),
        Felt::new(index as u64),
    );
    // request the initialization of the Merkle path verification
    expected *= mp_init.inv();

    let mp_verify_complete = HASH_CYCLE_LEN + (tree.depth() as usize) * HASH_CYCLE_LEN;
    let mp_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        // for the return hash, only the state digest matters, and it should match the root
        [
            ZERO,
            ZERO,
            ZERO,
            ZERO,
            tree.root()[0],
            tree.root()[1],
            tree.root()[2],
            tree.root()[3],
            ZERO,
            ZERO,
            ZERO,
            ZERO,
        ],
        [ZERO; STATE_WIDTH],
        Felt::new(mp_verify_complete as u64),
        Felt::new(index as u64 >> tree.depth()),
    );
    // request the result of the Merkle path verification
    expected *= mp_result.inv();
    assert_eq!(expected, b_chip[2]);

    // at cycle 2 the result of the span hash is requested by the decoder
    apply_permutation(&mut span_state);
    let span_result = build_expected(
        &alphas,
        RETURN_HASH_LABEL,
        span_state,
        [ZERO; STATE_WIDTH],
        Felt::new(8),
        ZERO,
    );
    expected *= span_result.inv();
    assert_eq!(expected, b_chip[3]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 3..8 {
        assert_eq!(expected, b_chip[row]);
    }

    // at cycle 7 the result of the span hash is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 7);
    assert_eq!(expected, b_chip[8]);

    // at cycle 8 the initialization of the merkle path is provided by the hasher
    expected *= build_expected_from_trace(&trace, &alphas, 8);
    assert_eq!(expected, b_chip[9]);

    // Nothing changes when there is no communication with the hash chiplet.
    for row in 10..(mp_verify_complete) {
        assert_eq!(expected, b_chip[row]);
    }

    // when the merkle path verification has been completed the hasher provides the result
    expected *= build_expected_from_trace(&trace, &alphas, mp_verify_complete - 1);
    assert_eq!(expected, b_chip[mp_verify_complete]);

    // The value in b_chip should be ONE now and for the rest of the trace.
    for row in mp_verify_complete..trace.length() - NUM_RAND_ROWS {
        assert_eq!(ONE, b_chip[row]);
    }
}

// TEST HELPERS
// ================================================================================================

/// Reduces the provided hasher row information to an expected value.
fn build_expected(
    alphas: &[Felt],
    label: u8,
    state: HasherState,
    next_state: HasherState,
    addr: Felt,
    index: Felt,
) -> Felt {
    let first_cycle_row = addr_to_cycle_row(addr) == 0;
    let transition_label = if first_cycle_row {
        label + 16_u8
    } else {
        label + 32_u8
    };
    let header =
        alphas[0] + alphas[1] * Felt::from(transition_label) + alphas[2] * addr + alphas[3] * index;
    let mut value = header;

    if (first_cycle_row && label == LINEAR_HASH_LABEL) || label == RETURN_STATE_LABEL {
        // include the entire state (words a, b, c)
        value += build_value(&alphas[4..16], &state);
    } else if label == LINEAR_HASH_LABEL {
        // include the delta between the next and current rate elements (words b and c)
        value += build_value(&alphas[8..16], &next_state[CAPACITY_LEN..]);
        value -= build_value(&alphas[8..16], &state[CAPACITY_LEN..]);
    } else if label == RETURN_HASH_LABEL {
        // include the digest (word b)
        value += build_value(&alphas[8..12], &state[DIGEST_RANGE]);
    } else {
        assert!(
            label == MP_VERIFY_LABEL
                || label == MR_UPDATE_NEW_LABEL
                || label == MR_UPDATE_OLD_LABEL
        );
        let bit = (index.as_int() >> 1) & 1;
        let left_word = build_value(&alphas[8..12], &state[DIGEST_RANGE]);
        let right_word = build_value(&alphas[8..12], &state[DIGEST_RANGE.end..]);

        value += Felt::new(1 - bit) * left_word + Felt::new(bit) * right_word;
    }

    value
}

/// Reduces the specified row in the execution trace to an expected value representing a hash
/// operation lookup.
fn build_expected_from_trace(trace: &ExecutionTrace, alphas: &[Felt], row: usize) -> Felt {
    let s0 = trace.main_trace.get_column(HASHER_TRACE_OFFSET)[row];
    let s1 = trace.main_trace.get_column(HASHER_TRACE_OFFSET + 1)[row];
    let s2 = trace.main_trace.get_column(HASHER_TRACE_OFFSET + 2)[row];
    let selectors: Selectors = [s0, s1, s2];

    let label = get_label_from_selectors(selectors)
        .expect("unrecognized hasher operation label in hasher trace");

    let addr = trace.main_trace.get_column(HASHER_ROW_COL_IDX)[row];
    let index = trace.main_trace.get_column(HASHER_NODE_INDEX_COL_IDX)[row];

    let cycle_row = addr_to_cycle_row(addr);

    let mut state = [ZERO; STATE_WIDTH];
    let mut next_state = [ZERO; STATE_WIDTH];
    for (i, col_idx) in HASHER_STATE_COL_RANGE.enumerate() {
        state[i] = trace.main_trace.get_column(col_idx)[row];
        if cycle_row == 7 && label == LINEAR_HASH_LABEL {
            // fill the next state with the elements being absorbed.
            next_state[i] = trace.main_trace.get_column(col_idx)[row + 1];
        }
    }

    build_expected(alphas, label, state, next_state, addr, index)
}

/// Builds a value from alphas and elements of matching lengths. This can be used to build the
/// value for a single word or for the entire state.
fn build_value(alphas: &[Felt], elements: &[Felt]) -> Felt {
    let mut value = ZERO;
    for (&alpha, &element) in alphas.iter().zip(elements.iter()) {
        value += alpha * element;
    }
    value
}

/// Returns the hash operation label for the specified selectors.
fn get_label_from_selectors(selectors: Selectors) -> Option<u8> {
    if selectors == LINEAR_HASH {
        Some(LINEAR_HASH_LABEL)
    } else if selectors == MP_VERIFY {
        Some(MP_VERIFY_LABEL)
    } else if selectors == MR_UPDATE_OLD {
        Some(MR_UPDATE_OLD_LABEL)
    } else if selectors == MR_UPDATE_NEW {
        Some(MR_UPDATE_NEW_LABEL)
    } else if selectors == RETURN_HASH {
        Some(RETURN_HASH_LABEL)
    } else if selectors == RETURN_STATE {
        Some(RETURN_STATE_LABEL)
    } else {
        None
    }
}

/// Populates the provided HasherState with the state stored in the decoder's execution trace at the
/// specified row.
fn fill_state_from_decoder(trace: &ExecutionTrace, state: &mut HasherState, row: usize) {
    for (i, col_idx) in DECODER_HASHER_STATE_RANGE.enumerate() {
        state[CAPACITY_LEN + i] = trace.main_trace.get_column(col_idx)[row];
    }
}

/// Absorbs the elements specified in the decoder's execution trace helper columns at the specified
/// row into the provided HasherState.
fn absorb_state_from_decoder(trace: &ExecutionTrace, state: &mut HasherState, row: usize) {
    for (i, col_idx) in DECODER_HASHER_STATE_RANGE.enumerate() {
        state[CAPACITY_LEN + i] += trace.main_trace.get_column(col_idx)[row];
    }
}

/// Returns the row of the hash cycle which corresponds to the provided Hasher address.
fn addr_to_cycle_row(addr: Felt) -> usize {
    let cycle = (addr.as_int() - 1) as usize;
    let cycle_row = cycle % HASH_CYCLE_LEN;
    debug_assert!(
        cycle_row == 0 || cycle_row == HASH_CYCLE_LEN - 1,
        "invalid address for hasher lookup"
    );

    cycle_row
}

/// Initializes Merkle tree leaves with the specified values.
fn init_leaves(values: &[u64]) -> Vec<Word> {
    values.iter().map(|&v| init_leaf(v)).collect()
}

/// Initializes a Merkle tree leaf with the specified value.
fn init_leaf(value: u64) -> Word {
    [Felt::new(value), Felt::ZERO, Felt::ZERO, Felt::ZERO]
}
