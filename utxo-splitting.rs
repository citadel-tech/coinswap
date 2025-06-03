use rand::{rng, Rng};

// Like the og function, but for playground a/b testing
pub fn generate_amount_fractions(count: usize, total_amount: u64) -> Vec<u64> {
    if count == 0 {
        return Vec::new();
    } else if count == 1 {
        return vec![total_amount];
    }

    let mut rng = rng();

    let mut cuts: Vec<u64> = (0..count - 1)
        .map(|_| rng.random_range(1..total_amount))
        .collect();

    cuts.sort_unstable();

    let mut output = Vec::with_capacity(count);
    let mut prev = 0;
    for &cut in &cuts {
        output.push(cut - prev);
        prev = cut;
    }
    output.push(total_amount - prev);

    output
}


// Change calculation flow:
// - We know the Target.
// - We run coinselection and get Input-Set-1, Min-Change.
// - We run the X algo, we find the extra required change. Total Change = min-change + change-delta.
// - We go back to input set, Input-Set-2, coin-selction of target value = change-delta. 
// - Total Input = Input-set-1 + input-set-2.
// - Target utxos chunked by Algo X.
// - Change chunks done by Algo X.


// Dynamic, simple and fast. Put some head to it.
// With random fees and scheduling, we would basically be able to mask sats with 5-10% differential range with the target vis change amount
// The idea is that with massive difference between change and target, we wont be able good enough splits in range of 5-6 destinations & change addrs.
// So, to obfuscate the transaction, I am matching changes with targets, at equal parity and letting the rest be flexible. 
// This way, we get the privacy we require by muddying the pool, we get that under the split number we need and it's dynamic enough to give the control to user ahead in the maturity of this project

/// Given two satoshi amounts `target` and `change`, pick splits n_target, n_change ∈ [2..=max_splits]

/// that minimize |(target / n_target) - (change / n_change)| (all integer division).
/// Returns (n_target, n_change, size_per_output_for_target, size_per_output_for_change, target_splits, change_splits).
fn pick_splits(
    target: u64,
    change: u64,
    max_splits: u64,
) -> (u64, u64, u64, u64, Vec<u64>, Vec<u64>) {
    // This here reverses and handles the case where change > target by swapping them
    let mut reversed = false;
    let (mut t, mut s) = (target, change);
    if change > target {
        reversed = true;
        t = change;
        s = target;
    }

    let mut best = (
        2,                                  // n_target
        2,                                  // n_change
        t / 2,                              // size_target
        s / 2,                              // size_change
        (t / 2).abs_diff(s / 2),           // diff
    );

    for n_t in 2..=max_splits {
        for n_s in 2..=max_splits {
            let avg_t = t / n_t;
            let avg_s = s / n_s;
            let diff = avg_t.abs_diff(avg_s);
            if diff < best.4 {
                best = (n_t, n_s, avg_t, avg_s, diff);
            }
            if diff == 0 {
                let target_splits = vec![avg_t; n_t as usize];
                let change_splits = vec![avg_s; n_s as usize];
                if reversed {
                    return (best.1, best.0, best.3, best.2, change_splits, target_splits);
                } else {
                    return (best.0, best.1, best.2, best.3, target_splits, change_splits);
                }
            }
        }
    }

    let mut target_splits = Vec::new();
    let mut change_splits = Vec::new();
    if best.4 * 10 > best.2 {
        let delta = best.2.saturating_sub(best.3);
        let mut acc = 0;
        for _ in 0..best.1 {
            let v = best.2.saturating_sub(delta);
            change_splits.push(v);
            acc += v;
        }
        let remaining = t.saturating_sub(acc);
        let more = generate_amount_fractions(
            (best.0 - best.1) as usize,
            remaining,
        );
        for v in more.iter().map(|&f| f as u64) {
            target_splits.push(v);
        }
        for _ in 0..best.1 {
            target_splits.push(best.3);
        }
    } else {
        target_splits  = vec![best.2; best.0 as usize];
        change_splits  = vec![best.3; best.1 as usize];
    }

    if reversed {
        (
            best.1,
            best.0,
            best.3,
            best.2,
            change_splits,
            target_splits,
        )
    } else {
        (
            best.0,
            best.1,
            best.2,
            best.3,
            target_splits,
            change_splits,
        )
    }
}

fn main() {
    let examples: &[(u64, u64)] = &[
        (10, 7),    // target=10 sats, change=7 sats
        (7, 10),    // target=7 sats,  change=10 sats (change > target)
        (5, 5),     // equal small
        (2, 1),     // very tiny
        (1, 3),     // change > target tiny
        (100, 20),  // small hundreds
        (20, 100),  // change > target hundreds
        (1_000, 500),
        (500, 1_000),
        (157_348, 642_193), // change > target
        (483_219, 213_576),
        (791_045, 186_724),
        (325_432, 918_576), // change > target
        (611_287, 389_614),
        (102_459, 207_816), // change > target
        (857_301, 727_408),
        (412_538, 995_137), // change > target
        (254_789, 254_321), // almost equal
        (999_047, 100_253), 
        (434_000_000,   70_000_000),
        (224_000_000,    3_000_000),
        (12_389_000_000, 7_845_000_000),
        (32_389_000_000, 1_245_000_000),
        (9_889_000_000,  6_745_000_000),
        (76_589_000_000,   245_000_000),
        (4_300_000_000,  800_000_000),
        (9_990_000_000, 5_550_000_000),
        (1_025_000_000,  475_000_000),
        (3_126_000_000,1_118_000_000),
        (7_550_000_000,1_000_000_000),
        (  400_000_000,  300_000_000),
        (  313_000_000,  123_200_000),
        (  600_000_000,  220_000_000),
        (  400_000_000,  400_000_000),
        (10_000_000_000,10_000_000_000), // 100.0 & 100.0
        (10_000_000_000, 9_990_000_000), // 100.0 &  99.9
        (100_000_000_000, 1_000_000_000),//1000.0 & 10.0
        ( 100_000_000,   50_000_000),   // 1.0 &  0.5
        (1_700_000_000,1_300_000_000),  //17.0 & 13.0
        (12_345_670_000,12_345_660_000),//123.4567 & 123.4566
        (10_000_000_000,2_500_000_000), //100.0 &  25.0
        (6_400_000_000,3_200_000_000),  //64.0 & 32.0
        (5_000_000_000,        0),      //50.0 &   0.0
        (         0,        0),        // 0.0 &   0.0
        (100_010_000,100_000_000),      //1.0001 & 1.0000
        (100_000_000_000_000, 100_000_000), //1_000_000.0 & 1.0
        (1_300_000_000,17_000_000_000),
    ];

    for &(a, b) in examples {
        let (n_a, n_b, size_a, size_b, target_splits, change_splits) =
            pick_splits(a, b, 5);

        // compute percentage diff as float
        let diff_pct = if size_a > 0 {
            ((size_a as f64 - size_b as f64).abs() * 100.0) / size_a as f64
        } else {
            0.0
        };

        println!(
            "a={} sats, b={} sats → splits ({}×{} sats) & ({}×{} sats) diff={:.5}%\n  target_splits: {:?}\n  change_splits: {:?}\n",
            a, b, n_a, size_a, n_b, size_b, diff_pct, target_splits, change_splits
        );
    }
}

// Target Amount: Variable 
// Input UTXOs: Variable,  [Target amount processed by coinselection]
// Target Outputs: 3 splits.
// - Number is fixed: Chunks = Target/number
// - Utxo amount is fixed: Number = Target/Chunks.

a=10 sats, b=7 sats → splits (3×3 sats) & (2×3 sats) diff=0.00000%
  target_splits: [3, 3, 3]
  change_splits: [3, 3]

a=7 sats, b=3 sats → splits (2×3 sats) & (3×3 sats) diff=0.00000%
  target_splits: [3, 3]
  change_splits: [3]

a=5 sats, b=5 sats → splits (2×2 sats) & (2×2 sats) diff=0.00000%
  target_splits: [2, 2]
  change_splits: [2, 2]

a=2 sats, b=1 sats → splits (3×0 sats) & (2×0 sats) diff=0.00000%
  target_splits: [0, 0, 0]
  change_splits: [0, 0]

a=1 sats, b=3 sats → splits (2×0 sats) & (4×0 sats) diff=0.00000%
  target_splits: [0, 0]
  change_splits: [0, 0, 0, 0]

a=100 sats, b=20 sats → splits (5×20 sats) & (2×10 sats) diff=50.00000%
  target_splits: [32, 26, 22, 10, 10]
  change_splits: [10, 10]

a=20 sats, b=100 sats → splits (2×10 sats) & (5×20 sats) diff=100.00000%
  target_splits: [10, 10]
  change_splits: [8, 16, 56, 10, 10]

a=1000 sats, b=500 sats → splits (4×250 sats) & (2×250 sats) diff=0.00000%
  target_splits: [250, 250, 250, 250]
  change_splits: [250, 250]

a=500 sats, b=1000 sats → splits (2×250 sats) & (4×250 sats) diff=0.00000%
  target_splits: [250, 250]
  change_splits: [250, 250, 250, 250]

a=157348 sats, b=642193 sats → splits (2×78674 sats) & (5×128438 sats) diff=63.25343%
  target_splits: [78674, 78674]
  change_splits: [213006, 192410, 79429, 78674, 78674]

a=483219 sats, b=213576 sats → splits (5×96643 sats) & (2×106788 sats) diff=10.49740%
  target_splits: [38506, 223933, 27494, 106788, 106788]
  change_splits: [96643, 96643]

a=791045 sats, b=186724 sats → splits (5×158209 sats) & (2×93362 sats) diff=40.98819%
  target_splits: [215169, 36569, 352583, 93362, 93362]
  change_splits: [93362, 93362]

a=325432 sats, b=918576 sats → splits (2×162716 sats) & (5×183715 sats) diff=12.90531%
  target_splits: [162716, 162716]
  change_splits: [325798, 20754, 246592, 162716, 162716]

a=611287 sats, b=389614 sats → splits (5×122257 sats) & (3×129871 sats) diff=6.22786%
  target_splits: [122257, 122257, 122257, 122257, 122257]
  change_splits: [129871, 129871, 129871]

a=102459 sats, b=207816 sats → splits (2×51229 sats) & (4×51954 sats) diff=1.41521%
  target_splits: [51229, 51229]
  change_splits: [51954, 51954, 51954, 51954]

a=857301 sats, b=727408 sats → splits (5×171460 sats) & (4×181852 sats) diff=6.06089%
  target_splits: [171460, 171460, 171460, 171460, 171460]
  change_splits: [181852, 181852, 181852, 181852]

a=412538 sats, b=995137 sats → splits (2×206269 sats) & (5×199027 sats) diff=3.51095%
  target_splits: [206269, 206269]
  change_splits: [199027, 199027, 199027, 199027, 199027]

a=254789 sats, b=254321 sats → splits (5×50957 sats) & (5×50864 sats) diff=0.18251%
  target_splits: [50957, 50957, 50957, 50957, 50957]
  change_splits: [50864, 50864, 50864, 50864, 50864]

a=999047 sats, b=100253 sats → splits (5×199809 sats) & (2×50126 sats) diff=74.91304%
  target_splits: [184365, 52485, 661945, 50126, 50126]
  change_splits: [50126, 50126]

a=434000000 sats, b=70000000 sats → splits (5×86800000 sats) & (2×35000000 sats) diff=59.67742%
  target_splits: [184637181, 111546721, 67816098, 35000000, 35000000]
  change_splits: [35000000, 35000000]

a=224000000 sats, b=3000000 sats → splits (5×44800000 sats) & (2×1500000 sats) diff=96.65179%
  target_splits: [898228, 185489711, 34612061, 1500000, 1500000]
  change_splits: [1500000, 1500000]

a=12389000000 sats, b=7845000000 sats → splits (5×2477800000 sats) & (3×2615000000 sats) diff=5.53717%
  target_splits: [2477800000, 2477800000, 2477800000, 2477800000, 2477800000]
  change_splits: [2615000000, 2615000000, 2615000000]

a=32389000000 sats, b=1245000000 sats → splits (5×6477800000 sats) & (2×622500000 sats) diff=90.39026%
  target_splits: [12275174527, 5923276373, 12945549100, 622500000, 622500000]
  change_splits: [622500000, 622500000]

a=9889000000 sats, b=6745000000 sats → splits (3×3296333333 sats) & (2×3372500000 sats) diff=2.31065%
  target_splits: [3296333333, 3296333333, 3296333333]
  change_splits: [3372500000, 3372500000]

a=76589000000 sats, b=245000000 sats → splits (5×15317800000 sats) & (2×122500000 sats) diff=99.20028%
  target_splits: [26471284223, 6565566810, 43307148967, 122500000, 122500000]
  change_splits: [122500000, 122500000]

a=4300000000 sats, b=800000000 sats → splits (5×860000000 sats) & (2×400000000 sats) diff=53.48837%
  target_splits: [1016578080, 1180193086, 1303228834, 400000000, 400000000]
  change_splits: [400000000, 400000000]

a=9990000000 sats, b=5550000000 sats → splits (5×1998000000 sats) & (3×1850000000 sats) diff=7.40741%
  target_splits: [1998000000, 1998000000, 1998000000, 1998000000, 1998000000]
  change_splits: [1850000000, 1850000000, 1850000000]

a=1025000000 sats, b=475000000 sats → splits (4×256250000 sats) & (2×237500000 sats) diff=7.31707%
  target_splits: [256250000, 256250000, 256250000, 256250000]
  change_splits: [237500000, 237500000]

a=3126000000 sats, b=1118000000 sats → splits (5×625200000 sats) & (2×559000000 sats) diff=10.58861%
  target_splits: [684256705, 415915552, 907827743, 559000000, 559000000]
  change_splits: [559000000, 559000000]

a=7550000000 sats, b=1000000000 sats → splits (5×1510000000 sats) & (2×500000000 sats) diff=66.88742%
  target_splits: [2447672608, 3190829881, 911497511, 500000000, 500000000]
  change_splits: [500000000, 500000000]

a=400000000 sats, b=300000000 sats → splits (4×100000000 sats) & (3×100000000 sats) diff=0.00000%
  target_splits: [100000000, 100000000, 100000000, 100000000]
  change_splits: [100000000, 100000000, 100000000]

a=313000000 sats, b=123200000 sats → splits (5×62600000 sats) & (2×61600000 sats) diff=1.59744%
  target_splits: [62600000, 62600000, 62600000, 62600000, 62600000]
  change_splits: [61600000, 61600000]

a=600000000 sats, b=220000000 sats → splits (5×120000000 sats) & (2×110000000 sats) diff=8.33333%
  target_splits: [120000000, 120000000, 120000000, 120000000, 120000000]
  change_splits: [110000000, 110000000]

a=400000000 sats, b=400000000 sats → splits (2×200000000 sats) & (2×200000000 sats) diff=0.00000%
  target_splits: [200000000, 200000000]
  change_splits: [200000000, 200000000]

a=10000000000 sats, b=10000000000 sats → splits (2×5000000000 sats) & (2×5000000000 sats) diff=0.00000%
  target_splits: [5000000000, 5000000000]
  change_splits: [5000000000, 5000000000]

a=10000000000 sats, b=9990000000 sats → splits (5×2000000000 sats) & (5×1998000000 sats) diff=0.10000%
  target_splits: [2000000000, 2000000000, 2000000000, 2000000000, 2000000000]
  change_splits: [1998000000, 1998000000, 1998000000, 1998000000, 1998000000]

a=100000000000 sats, b=1000000000 sats → splits (5×20000000000 sats) & (2×500000000 sats) diff=97.50000%
  target_splits: [21456972204, 13661469415, 63881558381, 500000000, 500000000]
  change_splits: [500000000, 500000000]

a=100000000 sats, b=50000000 sats → splits (4×25000000 sats) & (2×25000000 sats) diff=0.00000%
  target_splits: [25000000, 25000000, 25000000, 25000000]
  change_splits: [25000000, 25000000]

a=1700000000 sats, b=1300000000 sats → splits (4×425000000 sats) & (3×433333333 sats) diff=1.96078%
  target_splits: [425000000, 425000000, 425000000, 425000000]
  change_splits: [433333333, 433333333, 433333333]

a=12345670000 sats, b=12345660000 sats → splits (5×2469134000 sats) & (5×2469132000 sats) diff=0.00008%
  target_splits: [2469134000, 2469134000, 2469134000, 2469134000, 2469134000]
  change_splits: [2469132000, 2469132000, 2469132000, 2469132000, 2469132000]

a=10000000000 sats, b=2500000000 sats → splits (5×2000000000 sats) & (2×1250000000 sats) diff=37.50000%
  target_splits: [1294998448, 1947012892, 4257988660, 1250000000, 1250000000]
  change_splits: [1250000000, 1250000000]

a=6400000000 sats, b=3200000000 sats → splits (4×1600000000 sats) & (2×1600000000 sats) diff=0.00000%
  target_splits: [1600000000, 1600000000, 1600000000, 1600000000]
  change_splits: [1600000000, 1600000000]

a=5000000000 sats, b=0 sats → splits (5×1000000000 sats) & (2×0 sats) diff=100.00000%
  target_splits: [134304487, 1669087600, 3196607913, 0, 0]
  change_splits: [0, 0]

a=0 sats, b=0 sats → splits (2×0 sats) & (2×0 sats) diff=0.00000%
  target_splits: [0, 0]
  change_splits: [0, 0]

a=100010000 sats, b=100000000 sats → splits (5×20002000 sats) & (5×20000000 sats) diff=0.01000%
  target_splits: [20002000, 20002000, 20002000, 20002000, 20002000]
  change_splits: [20000000, 20000000, 20000000, 20000000, 20000000]

a=100000000000000 sats, b=100000000 sats → splits (5×20000000000000 sats) & (2×50000000 sats) diff=99.99975%
  target_splits: [47023481694662, 16670604261153, 36305814044185, 50000000, 50000000]
  change_splits: [50000000, 50000000]

a=1300000000 sats, b=17000000000 sats → splits (2×650000000 sats) & (5×3400000000 sats) diff=423.07692%
  target_splits: [650000000, 650000000]
  change_splits: [55066168, 14038646110, 1606287722, 650000000, 650000000]


Send
⏎
⋮