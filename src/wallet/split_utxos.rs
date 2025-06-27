use super::Wallet;
use crate::wallet::api::UTXOSpendInfo;
use bip39::rand::{thread_rng, Rng};
use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, RpcApi};

// Used for calculating fee optimization scores
struct FeeOptimizationResult {
    input: Vec<(ListUnspentResultEntry, UTXOSpendInfo)>,
    target_chunks: Vec<u64>,
    change_chunks: Vec<u64>,
    score: f64,
}

const MIN_TARGET_CHUNKS: usize = 2;
const MAX_SPLITS: usize = 5;

impl Wallet {
    /// Performs simple fee optimization based on the number of selected inputs and target chunks, change chunks
    fn simple_fee_optimization(
        &self,
        selected_inputs: Vec<(ListUnspentResultEntry, UTXOSpendInfo)>,
        target_chunks: Vec<u64>,
        change_chunks: Vec<u64>,
    ) -> f64 {
        // Set values.
        const INPUT_W: f64 = 2.0;
        const OUTPUT_TARGET_W: f64 = 1.0;
        const OUTPUT_CHANGE_W: f64 = 0.75;
        if target_chunks.len() == 1 && change_chunks.len() == 1 {
            // Single output, no fee optimization needed
            return f64::MAX;
        }
        selected_inputs.len() as f64 * INPUT_W
            + target_chunks.len() as f64 * OUTPUT_TARGET_W
            + change_chunks.len() as f64 * OUTPUT_CHANGE_W
    }

    // Adds multiplicative variation for randomness while maintaining approximate ratio structure
    // reference :- (ε, δ)-indistinguishable Mixing for Cryptocurrencies
    // https://eprint.iacr.org/2021/1197.pdf
    // Returns [total] with warning if num_chunks > 5
    fn vary_amounts(&self, total: u64, num_chunks: usize) -> Vec<u64> {
        let mut rng = thread_rng();

        match num_chunks {
            0 => vec![],
            1 => vec![total],
            2..=5 => {
                let ratios = match num_chunks {
                    2 => vec![1.05, 0.95],
                    3 => vec![1.0, 1.05, 0.95],
                    4 => vec![1.1, 0.9, 1.05, 0.95],
                    5 => vec![1.0, 1.1, 0.9, 1.05, 0.95],
                    _ => unreachable!(), // This line is safe because of the match guard
                };

                // Apply randomness (±5% of each ratio)
                let randomized: Vec<f64> = ratios
                    .iter()
                    .map(|&r| r * rng.gen_range(0.95..1.05))
                    .collect();

                // Normalize to maintain total
                let sum: f64 = randomized.iter().sum();
                let normalized: Vec<u64> = randomized
                    .iter()
                    .map(|&r| ((total as f64 * r / sum).round() as u64))
                    .collect();

                // Fix rounding errors
                let mut sum_check: i64 = normalized.iter().sum::<u64>() as i64;
                let mut adjusted = normalized.clone();

                while sum_check != total as i64 {
                    let idx = rng.gen_range(0..adjusted.len());
                    let delta = if sum_check < total as i64 { 1 } else { -1 };
                    adjusted[idx] = (adjusted[idx] as i64 + delta) as u64;
                    sum_check += delta;
                }

                // Random output ordering
                for i in 0..adjusted.len() {
                    let swap_with = rng.gen_range(0..adjusted.len());
                    adjusted.swap(i, swap_with);
                }

                adjusted
            }
            _ => {
                log::warn!(
                    "vary_amounts: num_chunks={num_chunks} exceeds maximum of 5, returning single chunk",
                );
                vec![total]
            }
        }
    }

    /// Tries to find the best way to split a target amount and change amount into chunks by iterating through
    /// the entire combination set and minimizing the relative difference between the average target chunk and change chunk.
    /// Leaves early after hitting 15% so as to not entertain unnecessarily large number of splits, saves fee.
    fn ct_harmony_split(
        &self,
        target: u64,
        target_change: u64,
        max_splits: usize,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut resulting_splits = (
            1,             // no. of target chunks
            1,             // no. of change chunks
            target,        // avg target chunk
            target_change, // avg change chunk
            (target).abs_diff(target_change) as f64 / (target).max(target_change) as f64, // relative_diff
            vec![target],        // target_chunks
            vec![target_change], // change_chunks
        );

        // Test all possible split combinations
        'outer: for n_t in 1..=max_splits {
            for n_s in 1..=max_splits {
                let avg_t = target / n_t as u64;
                let avg_s = target_change / n_s as u64;
                let diff = avg_t.abs_diff(avg_s);
                let max_avg = avg_t.max(avg_s) as f64;
                let relative_diff = diff as f64 / max_avg;

                // Update best if we find a better split
                if relative_diff < resulting_splits.4 {
                    resulting_splits = (
                        n_t,
                        n_s,
                        avg_t,
                        avg_s,
                        relative_diff,
                        vec![avg_t; n_t],
                        vec![avg_s; n_s],
                    );
                }

                // Early exit if we meet privacy threshold
                if relative_diff <= 0.15 {
                    break 'outer;
                }
            }
        }

        // 15% threshold, because beyond it, might as well use a single output
        // Else such utxo splitting may give way to a signature(tiny change/target utxo) for chain analysis.
        if resulting_splits.4 <= 0.15 {
            // Apply variability patterns
            let varied_targets = self.vary_amounts(target, resulting_splits.0);
            let varied_changes = self.vary_amounts(target_change, resulting_splits.1);
            (varied_targets, varied_changes)
        } else {
            (
                self.vary_amounts(target, MIN_TARGET_CHUNKS),
                vec![target_change],
            )
        }
    }

    /// Creates optimal transaction output splits for improved privacy
    /// Has 3 major cases and multiple sub-cases which optimize the transaction output splits(targets, change)
    /// And in conjunction with coin_selection, also tries to minimize the number of inputs used for fee optimization.
    /// This either creates dynamic splits with bounded, similar, randomized amounts OR gives a simple 1-1 output.
    /// It never creates a signature i.e a tiny change/target utxo, as that would be a privacy leak.
    pub fn create_dynamic_splits(
        &self,
        inital_selected_inputs: Vec<(ListUnspentResultEntry, UTXOSpendInfo)>,
        target: u64,
        fee_rate: f64,
    ) -> (
        Vec<(ListUnspentResultEntry, UTXOSpendInfo)>,
        Vec<u64>,
        Vec<u64>,
    ) {
        // 1. Select initial UTXOs
        let selected_inputs = inital_selected_inputs;

        let total_selected = selected_inputs.iter().collect::<Vec<_>>().iter().fold(
            Amount::ZERO,
            |acc, (unspent, _)| {
                acc.checked_add(unspent.amount)
                    .expect("Amount sum overflowed")
            },
        );

        // IMP: No need of this, already covered by coinselection. Keeping it for future debugging
        // if Amount::to_sat(total_selected) < target {
        //     panic!(
        //         "Insufficient funds: Needed {} sats, only have {}",
        //         target, total_selected
        //     );
        // }

        let target_change = Amount::to_sat(total_selected).saturating_sub(target);
        // IMP : This is anyway implicitly checked in the coin selection, but this is imortant to ensure we return total selected and not the entire target
        // It's possible to do below in the 3 cases but it would just be more useless code.
        if target_change == 0 {
            return (
                selected_inputs,
                vec![Amount::to_sat(total_selected)],
                vec![0],
            );
        };

        // Range of +- 10% of target for target_change sizes
        let target_lb = (target as f64 * 0.9) as u64;
        let target_ub = (target as f64 * 1.1) as u64;

        // === Case A: Change is within 10% of target (ideal) ===
        if (target_lb..=target_ub).contains(&target_change) {
            return (
                selected_inputs,
                self.vary_amounts(target, 2),
                self.vary_amounts(target_change, 2),
            );
        }

        // === Case B: Change too small (<90% of target) ===
        if target_change < target_lb {
            // Below is a special case which might to coinselection running thrice
            // Fee Optimization: If change is too small, we try to create a smaller delta_C
            // And get 2-1 split thus saving on the fee

            // if target_change < target_ub / 2 {
            //     let delta_c = target_ub / 2 - target_change;
            //     let delta_inputs = match self.coin_select(Amount::from_sat(delta_c), fee_rate) {
            //         Ok(inputs) => inputs,
            //         Err(e) => {
            //             log::error!("Error during coin selection: {e:?}");
            //             return (vec![], vec![], vec![]);
            //         }
            //     };

            //     let delta_input_sum =
            //         delta_inputs.iter().fold(Amount::ZERO, |acc, (unspent, _)| {
            //             acc.checked_add(unspent.amount)
            //                 .expect("Amount sum overflowed")
            //         });

            //     let delta_cc = Amount::to_sat(delta_input_sum).saturating_sub(delta_c);

            //     if delta_cc > 0
            //         && (target_lb / 2..=target_ub / 2)
            //             .contains(&(target_change + Amount::to_sat(delta_input_sum)))
            //     {
            //         return (
            //             selected_inputs,
            //             self.vary_amounts(target, 2),
            //             vec![target_change + Amount::to_sat(delta_input_sum)],
            //         );
            //     }
            //     if delta_cc > 0
            //         && (target_lb..=target_ub)
            //             .contains(&(target_change + Amount::to_sat(delta_input_sum)))
            //     {
            //         return (
            //             selected_inputs,
            //             self.vary_amounts(target, 2),
            //             self.vary_amounts(target_change + delta_input_sum.to_sat(), 2),
            //         );
            //     } else {
            //         let outpoints = delta_inputs
            //             .iter()
            //             .map(|(unspent, _)| OutPoint::new(unspent.txid, unspent.vout))
            //             .collect::<Vec<_>>();
            //         if let Err(e) = self.rpc.unlock_unspent(&outpoints) {
            //             log::info!("Failed to unlock unspent outputs: {e:?}");
            //         }
            //     }
            // }

            // Delta_c finds the threshold, which is used for a second coinselection
            let delta_c = target - target_change;
            let delta_inputs = match self.coin_select(Amount::from_sat(delta_c), fee_rate) {
                Ok(inputs) => inputs,
                Err(e) => {
                    log::info!("Error other than insufficient amount during second coin selection in dynamic splitting logic: {e:?}, backtracking and returning previous state");
                    let (target_chunks, change_chunks) =
                        self.ct_harmony_split(target, target_change, MAX_SPLITS);
                    return (selected_inputs, target_chunks, change_chunks);
                }
            };

            // Lock the outpoints here
            let outpoints = delta_inputs
                .iter()
                .map(|(unspent, _)| bitcoin::OutPoint::new(unspent.txid, unspent.vout))
                .collect::<Vec<_>>();

            self.rpc.lock_unspent(&outpoints).unwrap();

            // let delta_input_sum: u64 = delta_inputs.iter().sum();
            let delta_input_sum = delta_inputs.iter().fold(Amount::ZERO, |acc, (unspent, _)| {
                acc.checked_add(unspent.amount)
                    .expect("Amount sum overflowed")
            });

            let delta_cc = Amount::to_sat(delta_input_sum).saturating_sub(delta_c);

            // If delta_cc is small enough, we can distribute in a 2-2 split.
            if delta_cc > 0
                && (target_lb..=target_ub)
                    .contains(&(target_change + Amount::to_sat(delta_input_sum)))
            {
                return (
                    [&selected_inputs[..], &delta_inputs[..]].concat(),
                    self.vary_amounts(target, 2),
                    self.vary_amounts(target_change + delta_input_sum.to_sat(), 2),
                );
            }
            // If delta_cc is too massive, it will leave a trail output behind, so we go here instead
            else {
                // Fee Optimization : Here, we are comparing if it's beneficial to use the new delta inputs or not by looking at the fee optimization score
                // If it's not useful, it simply backtracks and uses the previous coins only.
                let (target_chunks1, change_chunks1) =
                    self.ct_harmony_split(target, target_change, MAX_SPLITS);

                let (target_chunks2, change_chunks2) = self.ct_harmony_split(
                    target,
                    target_change + Amount::to_sat(delta_input_sum),
                    MAX_SPLITS,
                );

                let results = [
                    FeeOptimizationResult {
                        input: selected_inputs.clone(),
                        target_chunks: target_chunks1.clone(),
                        change_chunks: change_chunks1.clone(),
                        score: self.simple_fee_optimization(
                            selected_inputs.clone(),
                            target_chunks1,
                            change_chunks1,
                        ),
                    },
                    FeeOptimizationResult {
                        input: [&selected_inputs[..], &delta_inputs[..]].concat(),
                        target_chunks: target_chunks2.clone(),
                        change_chunks: change_chunks2.clone(),
                        score: self.simple_fee_optimization(
                            [&selected_inputs[..], &delta_inputs[..]].concat(),
                            target_chunks2,
                            change_chunks2,
                        ),
                    },
                ];

                // debug
                log::info!(
                    "\nFee optimization results: {:?}\ntarget_chunks: {:?}\nchange_chunks: {:?}",
                    results.iter().map(|r| r.score).collect::<Vec<_>>(),
                    results.iter().map(|r| &r.target_chunks).collect::<Vec<_>>(),
                    results.iter().map(|r| &r.change_chunks).collect::<Vec<_>>()
                );
                // debug

                // Find the optimal case with the lowest score and returns it.
                let optimal_case = results
                    .iter()
                    .min_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
                    .expect("At least one result should exist");

                return (
                    optimal_case.input.clone(),
                    optimal_case.target_chunks.clone(),
                    optimal_case.change_chunks.clone(),
                );
            }
        }

        // === Case C: Change too large (>110% of target) ===
        if target_change > target_ub {
            let (target_chunks, change_chunks) =
                self.ct_harmony_split(target, target_change, MAX_SPLITS);
            return (selected_inputs, target_chunks, change_chunks);
        }

        // Fallback to simple transaction if no good split found
        (
            selected_inputs,
            self.vary_amounts(target, MIN_TARGET_CHUNKS),
            vec![total_selected.to_sat() - target],
        )
    }
}
