# Maker Selection Process

This section outlines how a taker selects makers for a swap and under what conditions a maker is marked as "bad."

## Maker Selection Criteria

The taker selects a maker by filtering the available pool based on the following criteria.

1. **Swap Size Compatibility**: The maker's advertised `min_size` and `max_size` must be compatible with the swap amount.
2. **Exclusion of Unsuitable Makers**: The taker excludes:
   - Makers it is currently swapping with.
   - Makers present in the `bad_makers` list.

### Conditions for Marking a Maker as "Bad"

A maker is added to the `bad_makers` list if they fail to cooperate during the swap protocol. This prevents the taker from selecting them for future swaps.

A maker is marked as "bad" under the following circumstances:

- **Fidelity Proof Verification Failure**: The maker's fidelity bond fails verification.
- **Failure to Provide Signatures**: The maker does not provide the required signatures for the contract transaction during the swap.
- **Funding Timeout**: The maker’s funding transaction does not appear in the mempool or get confirmed within the expected time.
- **Settlement Failure**: The maker is unresponsive or uncooperative during the final settlement phase.
- **Malicious Behavior**: The maker broadcasts the contract transaction prematurely, which is considered malicious behavior.
