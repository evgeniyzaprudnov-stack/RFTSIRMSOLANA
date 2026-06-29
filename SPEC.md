# UltraCore Rift — Engineering Specification

**Version:** 1.0 RC  
**Status:** Release Candidate — Open Technical Review  
**Programs:** `ultra_core_rift`, `rift_token`

---

## 1. Overview

UltraCore Rift is a two-program Solana protocol built with the Anchor framework. It implements a participant-based supply distribution model driven by a global scalar field. The core program maintains a strictly enforced economic invariant over all state transitions. A separate token program provides an SPL token layer on top of the core without coupling to or modifying core physics.

The architecture separates concerns into two layers:

- **Core** (`ultra_core_rift`): the mathematical layer. Owns and enforces the invariant. Has no knowledge of SPL tokens.
- **Token** (`rift_token`): the economic interface layer. Reads core state; mints SPL tokens in proportion to field conditions. Cannot violate the core invariant because it never writes to `CoreState`.

---

## 2. Mathematical Model

### 2.1 Participants and Balances

Let `p` be the number of registered participants. Each participant `i` holds an internal balance called `base_balance[i]` (type `i128`, can be negative down to the debt limit).

### 2.2 Global Field

`global_field` (type `i128`) is a scalar that is added uniformly to the effective balance of every participant. The *effective balance* of participant `i` is:

```
effective[i] = base_balance[i] + global_field
```

`global_field` is incremented by redistribution and decremented by negative entropy.

### 2.3 Core Invariant

At all times, the following identity must hold:

```
total_supply = total_base_sum + global_field × p
```

where:

- `total_supply` (`u128`): total units in existence, bounded by `i128::MAX`.
- `total_base_sum` (`i128`): sum of all `base_balance[i]` values.
- `global_field × p` (`i128`): the field contribution across all participants.

This is verified by `CoreState::check_invariant()` after every state-mutating instruction. Any transition that would violate it aborts with `InvariantViolation`.

An equivalent formulation tracks minting and burning:

```
total_supply = total_minted − total_burned
```

Both identities are checked on every call.

### 2.4 Dust Accumulator

When `amount` is redistributed among `p` participants, the per-participant increment is `q = floor(amount / p)`. The remainder `r = amount mod p` is held in `dust_accumulator` and added to the next redistribution:

```
total = amount + dust_accumulator
q = floor(total / p)
r = total mod p
dust_accumulator := r
```

The invariant enforces `dust_accumulator < p` at all times.

### 2.5 Debt Limit

To prevent unbounded negative balances, a dynamic floor is computed per instruction:

```
debt_limit = −(total_supply / (10 × p))
```

No participant may fall below `debt_limit` after a transfer. When `p = 0`, the protocol uses the constant floor `MIN_ABS_DEBT = −10^18`.

### 2.6 Negative Entropy

Negative entropy applies a fixed decay constant `NEG_E = −2,718,281,828,459,045,235` to `global_field` per tick:

```
global_field := global_field + NEG_E
total_base_sum := total_base_sum − (p × NEG_E)
```

The subtraction of `p × NEG_E` from `total_base_sum` is the invariant-preserving compensation: since `field_contrib = global_field × p` decreased by `p × NEG_E`, `total_base_sum` must increase by the same amount (recall `NEG_E` is negative, so `−(p × NEG_E)` is a positive addition).

The pre-condition `p ≤ NEG_E_MAX_P = i128::MAX / |NEG_E|` is checked before the multiply to prevent overflow.

### 2.7 Edge Costs

A directed edge from participant A to participant B carries weight `w` (type `i128`, `|w| ≤ MAX_EDGE_COST = 10^21`). During `transfer_with_edge`, `w` is deducted from the sender in addition to the transfer amount:

- `w > 0`: `w` units are burned from `total_supply`.
- `w < 0`: `|w|` units are minted into `total_supply`.
- `w = 0`: no-op (equivalent to plain transfer).

In all cases `total_base_sum` is adjusted by `−w` to preserve the invariant.

---

## 3. Architecture

### 3.1 Core Program (`ultra_core_rift`)

**Account types:**

| Account | Size (bytes) | Description |
|---|---|---|
| `CoreState` | 145 | Single global state account (not a PDA) |
| `UserAccount` | 56 | Per-participant account; PDA `["user", authority]` |
| `EdgeAccount` | 24 | Directed edge weight; PDA `["edge", from, to]` |

**Instructions:**

| Instruction | Authority | Description |
|---|---|---|
| `initialize` | payer | Creates `CoreState` with zero state |
| `set_paused` | gate | Pauses or unpauses all transfers |
| `register` | gate | Adds a participant; maintains invariant |
| `unregister` | gate | Removes a participant; burns positive balance |
| `transfer` | from_authority | Moves units between two participants |
| `transfer_with_edge` | from_authority | Transfer with directed edge cost |
| `set_edge` | gate | Creates or updates an edge weight |
| `redistribute` | gate | Increases `global_field`; mints supply |
| `apply_neg_entropy` | gate | Decreases `global_field`; adjusts base sum |

### 3.2 Token Program (`rift_token`)

**Account types:**

| Account | Size (bytes) | Description |
|---|---|---|
| `RiftTokenState` | 132 | Token config; PDA `["rift_token_state"]` |
| SPL Mint | — | Standard SPL mint; authority = PDA `["rift_mint_authority"]` |

**Instructions:**

| Instruction | Authority | Description |
|---|---|---|
| `initialize` | gate | Creates state; mints genesis allocation |
| `issue_rift` | user (pays SOL) | Mints RIFT shares based on field conditions |
| `rebase` | authority | Updates cached `rift_multiplier` |

### 3.3 Planner (not implemented)

Reserved for a future off-chain or on-chain component that batches redistribute and apply_neg_entropy calls according to a schedule.

### 3.4 Memory (not implemented)

Reserved for persistent historical field snapshots or per-participant audit trails.

---

## 4. Core Invariant (Detailed)

### 4.1 Formal Statement

```
Invariant I:
  (1) total_supply = total_base_sum + global_field × p
  (2) total_supply = total_minted − total_burned
  (3) total_supply ≤ i128::MAX
  (4) total_minted ≥ total_burned
  (5) if p > 0: dust_accumulator < p
```

### 4.2 Invariant Maintenance per Instruction

**register(user):**
```
total_base_sum := total_base_sum − global_field
p              := p + 1
```
Proof: new field_contrib = global_field × (p+1) = old_field_contrib + global_field.
Subtracting global_field from total_base_sum cancels the increase.

**unregister():**
```
total_base_sum := total_base_sum − base_balance[user] + global_field
p              := p − 1
total_burned   := total_burned + base_balance[user]  (if > 0)
total_supply   := total_supply − base_balance[user]  (if > 0)
```

**transfer(amount, edge_cost):**
```
base_balance[from] := base_balance[from] − amount − edge_cost
base_balance[to]   := base_balance[to]   + amount
total_base_sum     := total_base_sum − edge_cost
```
`total_base_sum` changes by `-edge_cost`; supply changes by `edge_cost` (burn) or `-edge_cost` (mint), keeping identity (1) intact.

**redistribute(amount):**
```
total          = amount + dust_accumulator
q              = floor(total / p)
r              = total mod p
global_field   := global_field + q
total_supply   := total_supply + q × p
total_minted   := total_minted + q × p
dust_accumulator := r
```

**apply_neg_entropy():**
```
delta          = p × NEG_E             (negative)
global_field   := global_field + NEG_E
total_base_sum := total_base_sum − delta
```
`field_contrib` changes by `NEG_E × p = delta`. Subtracting `delta` from `total_base_sum` (i.e., adding `|delta|`) preserves identity (1).

---

## 5. Security Model

### 5.1 Gate Authority

`CoreState.gate` is a `Pubkey` set at initialization. All state-mutating core instructions except `transfer` and `transfer_with_edge` require the gate as a signer, enforced via Anchor's `has_one = gate` constraint with error `UnauthorizedGate`.

### 5.2 Pause Mechanism

`CoreState.paused: bool` halts all transfer operations when true. The `set_paused` instruction is gate-only. The check is performed inside `perform_transfer`, which is the single code path for both `transfer` and `transfer_with_edge`. The token layer checks `core.paused` explicitly in `issue_rift` before any mutation.

### 5.3 PDA Validation

All PDAs are validated by Anchor's constraint system:

| PDA | Seeds | Usage |
|---|---|---|
| `rift_token_state` | `["rift_token_state"]` | Single token config |
| `rift_mint_authority` | `["rift_mint_authority"]` | SPL mint authority |
| `user_account` | `["user", authority]` | Per-user balance |
| `edge_account` | `["edge", from, to]` | Directed edge weight |

### 5.4 CoreState Address Binding

`RiftTokenState.core_state` stores the `Pubkey` of the bound `CoreState` at initialization. Every instruction in `rift_token` that accepts a `core_state` account verifies it against the stored address:

```rust
constraint = core_state.key() == rift_token_state.core_state
    @ TokenError::InvalidCoreState
```

This prevents account substitution attacks where a caller provides a fabricated `CoreState` with a manipulated `global_field`.

### 5.5 Admin Vault Binding

`RiftTokenState.admin_vault` is stored at initialization. `IssueRift` verifies the provided `admin_vault` matches:

```rust
constraint = admin_vault.key() == rift_token_state.admin_vault
    @ TokenError::InvalidAdminVault
```

### 5.6 Transfer Authority

In `transfer`, `from_authority` is a `Signer`. The `from_user` PDA is derived from `from_authority.key()`, so only the owner of `from_authority` can spend from that user account.

In `transfer_with_edge`, an additional check verifies that `to_user.authority == to_authority.key()` to prevent edge-routing to an unintended recipient account.

### 5.7 Arithmetic Safety

All arithmetic uses Rust's checked operations (`checked_add`, `checked_sub`, `checked_mul`, `checked_div`) returning `RiftError::MathOverflow` on failure. All `u128 → i128` and `u128 → u64` conversions use `try_into()` with explicit error propagation. Bare arithmetic casts (`as`) are used only where proof of safety is documented inline.

---

## 6. Economic Layer

### 6.1 Separation of Concerns

The token program is a read-only consumer of core state. It reads `CoreState.global_field` and `CoreState.paused`, calls `CoreState.check_invariant()`, but **never writes to `CoreState`**. The core invariant cannot be violated by the token layer.

The mint rate in `issue_rift` is a function of `global_field` but does not modify it. `rift_multiplier` is a cached off-chain convenience value updated by `rebase`; it has no on-chain effect on minting logic.

### 6.2 Mint Formula

```
field_pressure   = max(|global_field|, MIN_FIELD_PRESSURE)
mint_multiplier  = 1_000_000_000_000_000 / field_pressure
shares_to_mint   = (base_amount − fee) × mint_multiplier / 1_000_000_000_000
```

`MIN_FIELD_PRESSURE = 10^6` caps `mint_multiplier` at `10^9`. The formula is monotonically decreasing in `|global_field|`: higher field pressure → fewer shares per unit of base_amount.

### 6.3 Protocol Fee

```
fee = base_amount × fee_bps / 10_000
```

`fee_bps ≤ MAX_FEE_BPS = 10` (0.10% maximum). Fee is transferred in SOL to `admin_vault`. The SOL transfer and mint CPI occur within the same transaction; failure of either rolls back both.

### 6.4 Genesis Allocation

At initialization:

```
founder_share = initial_supply × FOUNDER_SHARE_BPS / 10_000
```

`FOUNDER_SHARE_BPS = 314` (3.14%). Minted to `admin_vault_token_account`.

---

## 7. Protocol Guarantees

The following properties hold as long as the implementation is correct and no validator-level exploit is present:

1. **Invariant integrity.** `total_supply = total_base_sum + global_field × p` is verified after every state-mutating instruction. Any violation aborts the transaction.

2. **No silent arithmetic.** All integer operations use checked arithmetic. Overflow or underflow aborts with `MathOverflow`.

3. **Debt floor.** No participant can acquire a balance below `debt_limit()`. Unregistration is blocked for participants with negative balances.

4. **Supply accounting.** `total_supply = total_minted − total_burned` is maintained as a second invariant check. Both quantities are monotonically non-decreasing.

5. **Dust conservation.** All redistribution remainders are accumulated and included in the next redistribution call. No units are created or destroyed by rounding.

6. **Access control.** All gate-privileged instructions require the gate `Signer`. All PDA-bound accounts are validated by seeds and bumps. CoreState and admin vault addresses are verified against values stored at initialization.

7. **Pause coverage.** When `paused = true`, all transfer and issuance operations are rejected. Gate-only instructions (redistribute, apply_neg_entropy, register, unregister) are not blocked by pause.

8. **Token isolation.** The token layer cannot modify `CoreState`. All token-side arithmetic is independent of core accounting.

---

## 8. Known Limitations

The following items are **not** implemented in v1.0 RC:

- **No governance.** The gate is a single keypair. There is no multisig, timelock, or on-chain governance mechanism for gate operations.

- **No oracle integration.** `global_field` is set exclusively by gate-controlled instructions (redistribute, apply_neg_entropy, set_edge). There is no trustless price or data feed.

- **No participant self-registration.** `register` and `unregister` are gate-only. Participants cannot join or leave without gate approval.

- **No transfer self-authorization without gate.** Users can sign their own transfers but cannot register themselves or modify edge weights.

- **`rift_multiplier` is advisory only.** It is a cached value updated by `rebase`. Off-chain consumers must call `rebase` to keep it current. On-chain minting logic always recomputes from `global_field` directly.

- **`total_shares` tracks token-layer mints only.** It does not reflect burns or SPL-level transfers. It is an accounting convenience, not an authoritative supply figure (the SPL mint supply is authoritative).

- **No maximum supply cap on the SPL mint.** `total_shares` is `u64`; `checked_add` will abort if it overflows, but the SPL mint itself has no protocol-enforced cap beyond the u64 maximum of the SPL standard.

- **Planner not implemented.** Scheduling of redistribute and apply_neg_entropy calls is entirely off-chain.

- **Memory not implemented.** No on-chain history or snapshot mechanism exists.

---

## 9. Future Extensions

### 9.1 Planner

An on-chain or off-chain scheduler that triggers `redistribute` and `apply_neg_entropy` at defined intervals or based on observable conditions. Would reduce reliance on the gate for routine field updates.

### 9.2 Memory

A snapshot account storing periodic `(global_field, total_supply, timestamp)` tuples, enabling on-chain proofs of historical state for bridges or derivatives.

### 9.3 Governance

A multisig or DAO-controlled gate using existing Solana governance programs (e.g. SPL Governance). Would eliminate the single-keypair trust assumption on gate operations.

### 9.4 Oracle Integration

A permissionless instruction that allows a trusted oracle to increment `global_field` based on external data (e.g. fee revenue, protocol metrics), reducing gate centralization.

---

## 10. Conclusion

UltraCore Rift v1.0 RC is a deterministic, invariant-enforced supply distribution protocol. The core mathematical model is implemented without floating-point arithmetic; all operations use 128-bit integer arithmetic with explicit overflow checks. The token layer is a thin issuance interface that reads but never modifies core state.

The primary engineering risk at this stage is operational centralization: the gate keypair is the single authority for all privileged operations. This is a known and documented limitation, not a code defect. The on-chain arithmetic and invariant enforcement are ready for external technical review.
