use anchor_lang::prelude::*;

declare_id!("Fg6PaFpoGXkYsidMpWxTWqkYqk5Nnq4P6A4jR4Jm5Y8A");

// ============================================================================
// CONSTANTS
// ============================================================================

pub const NEG_E: i128 = -2_718_281_828_459_045_235;
pub const MAX_PARTICIPANTS: u64 = 1_000_000_000_000;
pub const MAX_EDGE_COST: i128 = 1_000_000_000_000_000_000_000;
pub const MIN_ABS_DEBT: i128 = -1_000_000_000_000_000_000;
pub const MAX_SUPPLY: u128 = i128::MAX as u128;

/// Compile-time ceiling: if p exceeds this value, p * NEG_E overflows i128.
/// Computed as i128::MAX / |NEG_E| = i128::MAX / 2_718_281_828_459_045_235.
/// In practice MAX_PARTICIPANTS (1e12) is far below this (~6.26e19), so
/// the guard in apply_neg_entropy is belt-and-suspenders.
pub const NEG_E_MAX_P: i128 = i128::MAX / (-NEG_E);

// ============================================================================
// PROGRAM
// ============================================================================

#[program]
pub mod ultra_core_rift {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, gate: Pubkey) -> Result<()> {
        let state = &mut ctx.accounts.core_state;
        *state = CoreState {
            gate,
            paused: false,
            global_field: 0,
            total_base_sum: 0,
            total_supply: 0,
            total_minted: 0,
            total_burned: 0,
            p: 0,
            dust_accumulator: 0,
        };
        state.check_invariant()
    }

    /// Gate-only: pause or unpause all transfer operations.
    pub fn set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
        ctx.accounts.core_state.paused = paused;
        emit!(PausedEvent { paused });
        Ok(())
    }

    /// Gate-only: set or update the weight of a directed edge between two participants.
    pub fn set_edge(
        ctx: Context<SetEdge>,
        _from: Pubkey,
        _to: Pubkey,
        weight: i128,
    ) -> Result<()> {
        require!(
            weight >= -MAX_EDGE_COST && weight <= MAX_EDGE_COST,
            RiftError::EdgeLimitExceeded
        );
        ctx.accounts.edge_account.weight = weight;
        Ok(())
    }

    /// Gate-only: register a new participant.
    ///
    /// Invariant sync: the new participant starts with base_balance = 0.
    /// Their field_contrib = global_field * 1 will be added to the sum when
    /// p increments. To keep total_supply = total_base_sum + field_contrib(all p)
    /// unchanged, we pre-subtract global_field from total_base_sum.
    pub fn register(ctx: Context<Register>, user: Pubkey) -> Result<()> {
        let state = &mut ctx.accounts.core_state;
        require!(state.p < MAX_PARTICIPANTS, RiftError::MaxParticipantsReached);

        let user_account = &mut ctx.accounts.user_account;
        user_account.authority = user;
        user_account.base_balance = 0;

        state.total_base_sum = state.total_base_sum
            .checked_sub(state.global_field)
            .ok_or(RiftError::MathOverflow)?;

        state.p = state.p.checked_add(1).ok_or(RiftError::MathOverflow)?;

        emit!(RegisteredEvent { user });
        state.check_invariant()
    }

    /// Gate-only: unregister a participant and burn any remaining positive balance.
    ///
    /// Invariant sync: remove this participant's base_balance from total_base_sum,
    /// then re-add global_field to compensate for p decrementing by 1.
    pub fn unregister(ctx: Context<Unregister>) -> Result<()> {
        let state = &mut ctx.accounts.core_state;
        let base = ctx.accounts.user_account.base_balance;

        require!(base >= 0, RiftError::DebtOnExitNotAllowed);

        if base > 0 {
            // base > 0 and base is i128, so the cast to u128 is safe.
            let burn = base as u128;
            require!(state.total_supply >= burn, RiftError::SupplyUnderflow);

            state.total_supply = state.total_supply
                .checked_sub(burn)
                .ok_or(RiftError::MathOverflow)?;
            state.total_burned = state.total_burned
                .checked_add(burn)
                .ok_or(RiftError::MathOverflow)?;

            emit!(BurnEvent {
                user: ctx.accounts.user_account.authority,
                amount: burn,
            });
        }

        state.total_base_sum = state.total_base_sum
            .checked_sub(base)
            .ok_or(RiftError::MathOverflow)?
            .checked_add(state.global_field)
            .ok_or(RiftError::MathOverflow)?;

        state.p = state.p.checked_sub(1).ok_or(RiftError::MathOverflow)?;

        emit!(UnregisteredEvent {
            user: ctx.accounts.user_account.authority,
        });
        state.check_invariant()
    }

    /// Transfer amount from signer's account to to_authority's account, no edge cost.
    pub fn transfer(ctx: Context<Transfer>, amount: u128) -> Result<()> {
        ctx.accounts.transfer_ctx.perform_transfer(amount, 0)
    }

    /// Transfer amount via a directed edge; edge weight is applied as a cost to sender.
    pub fn transfer_with_edge(ctx: Context<TransferWithEdge>, amount: u128) -> Result<()> {
        let edge_cost = ctx.accounts.edge_account.weight;

        // Verify that the to_user account actually belongs to to_authority.
        require_keys_eq!(
            ctx.accounts.transfer_ctx.to_user.authority,
            ctx.accounts.transfer_ctx.to_authority.key(),
            RiftError::UnauthorizedAuthority
        );

        ctx.accounts.transfer_ctx.perform_transfer(amount, edge_cost)
    }

    /// Gate-only: distribute amount evenly among all participants by incrementing
    /// global_field. Remainder is held in dust_accumulator for the next call.
    pub fn redistribute(ctx: Context<Redistribute>, amount: u128) -> Result<()> {
        let state = &mut ctx.accounts.core_state;
        require!(state.p > 0, RiftError::ZeroParticipants);

        let p_u128 = state.p as u128;

        let total = amount
            .checked_add(state.dust_accumulator)
            .ok_or(RiftError::MathOverflow)?;

        let q = total.checked_div(p_u128).ok_or(RiftError::MathOverflow)?;
        let r = total.checked_rem(p_u128).ok_or(RiftError::MathOverflow)?;

        // q can be up to total (when p = 1), which is u128. global_field is i128,
        // so we need a checked downcast. q > i128::MAX would mean the single
        // redistribution tick exceeds the entire signed field range, which the
        // caller controls and should never reach in practice.
        let q_i128: i128 = q.try_into().map_err(|_| RiftError::MathOverflow)?;

        state.global_field = state.global_field
            .checked_add(q_i128)
            .ok_or(RiftError::MathOverflow)?;

        // distributed = q * p <= total <= u128::MAX; checked_mul is safe but
        // kept for defensive clarity.
        let distributed = q.checked_mul(p_u128).ok_or(RiftError::MathOverflow)?;

        state.total_supply = state.total_supply
            .checked_add(distributed)
            .ok_or(RiftError::MathOverflow)?;
        state.total_minted = state.total_minted
            .checked_add(distributed)
            .ok_or(RiftError::MathOverflow)?;

        state.dust_accumulator = r;

        emit!(RedistributeEvent {
            amount,
            per_user: q,
            dust_retained: r,
        });
        emit!(FieldUpdateEvent {
            new_global_field: state.global_field,
        });

        state.check_invariant()
    }

    /// Gate-only: apply one negative entropy tick. Decrements global_field by |NEG_E|,
    /// adjusting total_base_sum to keep total_supply invariant.
    pub fn apply_neg_entropy(ctx: Context<ApplyNegEntropy>) -> Result<()> {
        let state = &mut ctx.accounts.core_state;

        let p_i128 = state.p as i128;

        // Guard before the multiply: if p exceeds NEG_E_MAX_P, p * NEG_E overflows.
        // This check is compile-time free (NEG_E_MAX_P is a const).
        require!(p_i128 <= NEG_E_MAX_P, RiftError::PhysicalOverflowLimit);

        // delta = p * NEG_E (negative). Used solely to adjust total_base_sum.
        let delta = p_i128
            .checked_mul(NEG_E)
            .ok_or(RiftError::MathOverflow)?;

        // global_field shifts by one NEG_E tick, independent of p.
        state.global_field = state.global_field
            .checked_add(NEG_E)
            .ok_or(RiftError::MathOverflow)?;

        // field_contrib = global_field * p changes by NEG_E * p = delta (negative).
        // To keep total_supply = total_base_sum + field_contrib unchanged,
        // subtract delta. Since delta < 0, checked_sub(delta) effectively adds |delta|.
        state.total_base_sum = state.total_base_sum
            .checked_sub(delta)
            .ok_or(RiftError::MathOverflow)?;

        emit!(FieldUpdateEvent {
            new_global_field: state.global_field,
        });
        state.check_invariant()
    }
}

// ============================================================================
// CORE STATE
// ============================================================================

#[account]
pub struct CoreState {
    pub gate: Pubkey,           // 32
    pub paused: bool,           //  1
    pub global_field: i128,     // 16
    pub total_base_sum: i128,   // 16
    pub total_supply: u128,     // 16
    pub total_minted: u128,     // 16
    pub total_burned: u128,     // 16
    pub p: u64,                 //  8
    pub dust_accumulator: u128, // 16
}                               // = 137 data bytes

impl CoreState {
    /// On-chain allocation: 8 (Anchor discriminator) + 137 (data) = 145 bytes.
    ///
    /// Field breakdown:
    ///   gate(32) + paused(1) + 6×i128/u128(96) + p(8) = 137
    ///
    /// The six 16-byte fields are:
    ///   global_field, total_base_sum, total_supply, total_minted,
    ///   total_burned, dust_accumulator.
    pub const SPACE: usize = 8 + 32 + 1 + 16 * 6 + 8; // = 145

    /// Dynamic debt floor: -total_supply / (10 * p).
    /// When p = 0 returns the protocol minimum MIN_ABS_DEBT.
    pub fn debt_limit(&self) -> Result<i128> {
        let factor = (self.p as i128)
            .checked_mul(10)
            .ok_or(RiftError::MathOverflow)?;

        if factor == 0 {
            return Ok(MIN_ABS_DEBT);
        }

        // total_supply <= MAX_SUPPLY = i128::MAX, so the cast is safe.
        let limit = (self.total_supply as i128)
            .checked_div(factor)
            .ok_or(RiftError::MathOverflow)?;
        Ok(-limit)
    }

    /// Verify the core economic invariant:
    ///   total_supply == total_base_sum + global_field * p
    ///   total_supply == total_minted - total_burned
    ///   dust_accumulator < p  (when p > 0)
    pub fn check_invariant(&self) -> Result<()> {
        // total_supply is bounded to i128::MAX so it can be compared as i128.
        require!(self.total_supply <= MAX_SUPPLY, RiftError::MathOverflow);

        let field_contrib = self.global_field
            .checked_mul(self.p as i128)
            .ok_or(RiftError::MathOverflow)?;

        let expected = self.total_base_sum
            .checked_add(field_contrib)
            .ok_or(RiftError::MathOverflow)?;

        // total_supply <= i128::MAX, so the cast is lossless.
        let supply_signed = self.total_supply as i128;
        require!(supply_signed == expected, RiftError::InvariantViolation);

        require!(
            self.total_minted >= self.total_burned,
            RiftError::InvariantViolation
        );
        // Use checked_sub: even though the require! above makes underflow
        // impossible, bare subtraction wraps silently in release mode.
        let net_supply = self.total_minted
            .checked_sub(self.total_burned)
            .ok_or(RiftError::MathOverflow)?;
        require!(self.total_supply == net_supply, RiftError::InvariantViolation);

        if self.p > 0 {
            require!(
                self.dust_accumulator < self.p as u128,
                RiftError::InvariantViolation
            );
        }
        Ok(())
    }
}

// ============================================================================
// TRANSFER LOGIC
// ============================================================================

#[derive(Accounts)]
pub struct TransferCtx<'info> {
    #[account(mut)]
    pub core_state: Account<'info, CoreState>,
    #[account(mut, seeds = [b"user", from_authority.key().as_ref()], bump)]
    pub from_user: Account<'info, UserAccount>,
    #[account(mut, seeds = [b"user", to_authority.key().as_ref()], bump)]
    pub to_user: Account<'info, UserAccount>,
    pub from_authority: Signer<'info>,
    /// CHECK: Used only as the PDA seed for to_user. Ownership verified via
    /// to_user.authority == to_authority in transfer_with_edge.
    pub to_authority: UncheckedAccount<'info>,
}

impl<'info> TransferCtx<'info> {
    pub fn perform_transfer(&mut self, amount: u128, edge_cost: i128) -> Result<()> {
        let state = &mut self.core_state;
        require!(!state.paused, RiftError::ProtocolPaused);

        if amount == 0 {
            return Ok(());
        }

        // amount is u128; base_balance is i128. Values above i128::MAX would
        // exceed MAX_SUPPLY and should never exist in a valid state.
        let amt: i128 = amount.try_into().map_err(|_| RiftError::MathOverflow)?;

        let new_from = self.from_user.base_balance
            .checked_sub(amt)
            .ok_or(RiftError::MathOverflow)?
            .checked_sub(edge_cost)
            .ok_or(RiftError::MathOverflow)?;

        require!(new_from >= state.debt_limit()?, RiftError::DebtLimitExceeded);

        self.from_user.base_balance = new_from;
        self.to_user.base_balance = self.to_user.base_balance
            .checked_add(amt)
            .ok_or(RiftError::MathOverflow)?;

        if edge_cost != 0 {
            // total_base_sum must track the removal of edge_cost from circulation.
            state.total_base_sum = state.total_base_sum
                .checked_sub(edge_cost)
                .ok_or(RiftError::MathOverflow)?;

            match edge_cost.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    // Positive edge cost: burned from supply.
                    // edge_cost > 0 and edge_cost is i128, so cast to u128 is safe.
                    let burn = edge_cost as u128;
                    require!(state.total_supply >= burn, RiftError::SupplyUnderflow);
                    state.total_supply = state.total_supply
                        .checked_sub(burn)
                        .ok_or(RiftError::MathOverflow)?;
                    state.total_burned = state.total_burned
                        .checked_add(burn)
                        .ok_or(RiftError::MathOverflow)?;
                    emit!(BurnEvent {
                        user: self.from_user.authority,
                        amount: burn,
                    });
                }
                std::cmp::Ordering::Less => {
                    // Negative edge cost: minted into supply.
                    // edge_cost < 0 and edge_cost >= -MAX_EDGE_COST > i128::MIN,
                    // so negation and cast to u128 are both safe.
                    let mint = (-edge_cost) as u128;
                    state.total_supply = state.total_supply
                        .checked_add(mint)
                        .ok_or(RiftError::MathOverflow)?;
                    state.total_minted = state.total_minted
                        .checked_add(mint)
                        .ok_or(RiftError::MathOverflow)?;
                    emit!(MintEvent {
                        user: self.from_user.authority,
                        amount: mint,
                    });
                }
                _ => {}
            }
        }

        emit!(TransferEvent {
            from: self.from_user.authority,
            to: self.to_user.authority,
            amount,
        });

        state.check_invariant()
    }
}

// ============================================================================
// ACCOUNT STRUCTS
// ============================================================================

#[account]
pub struct UserAccount {
    pub authority: Pubkey,  // 32
    pub base_balance: i128, // 16
}
impl UserAccount {
    pub const SPACE: usize = 8 + 32 + 16; // = 56
}

#[account]
pub struct EdgeAccount {
    pub weight: i128, // 16
}
impl EdgeAccount {
    pub const SPACE: usize = 8 + 16; // = 24
}

// ============================================================================
// INSTRUCTION CONTEXTS
// ============================================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = payer, space = CoreState::SPACE)]
    pub core_state: Account<'info, CoreState>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetPaused<'info> {
    #[account(mut, has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    pub gate: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(user: Pubkey)]
pub struct Register<'info> {
    #[account(mut, has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    #[account(
        init,
        payer = gate,
        space = UserAccount::SPACE,
        seeds = [b"user", user.as_ref()],
        bump
    )]
    pub user_account: Account<'info, UserAccount>,
    #[account(mut)]
    pub gate: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Unregister<'info> {
    #[account(mut, has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    #[account(
        mut,
        close = gate,
        seeds = [b"user", user_account.authority.as_ref()],
        bump
    )]
    pub user_account: Account<'info, UserAccount>,
    #[account(mut)]
    pub gate: Signer<'info>,
}

#[derive(Accounts)]
pub struct Transfer<'info> {
    pub transfer_ctx: TransferCtx<'info>,
}

#[derive(Accounts)]
pub struct TransferWithEdge<'info> {
    pub transfer_ctx: TransferCtx<'info>,
    #[account(
        seeds = [
            b"edge",
            transfer_ctx.from_authority.key().as_ref(),
            transfer_ctx.to_authority.key().as_ref(),
        ],
        bump
    )]
    pub edge_account: Account<'info, EdgeAccount>,
}

#[derive(Accounts)]
#[instruction(from: Pubkey, to: Pubkey)]
pub struct SetEdge<'info> {
    #[account(has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    #[account(
        init_if_needed,
        payer = gate,
        space = EdgeAccount::SPACE,
        seeds = [b"edge", from.as_ref(), to.as_ref()],
        bump
    )]
    pub edge_account: Account<'info, EdgeAccount>,
    #[account(mut)]
    pub gate: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Redistribute<'info> {
    #[account(mut, has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    pub gate: Signer<'info>,
}

#[derive(Accounts)]
pub struct ApplyNegEntropy<'info> {
    #[account(mut, has_one = gate @ RiftError::UnauthorizedGate)]
    pub core_state: Account<'info, CoreState>,
    pub gate: Signer<'info>,
}

// ============================================================================
// ERRORS
// ============================================================================

#[error_code]
pub enum RiftError {
    #[msg("Critical: The core economic invariant has been violated.")]
    InvariantViolation,
    #[msg("Unauthorized: Caller is not the designated gate.")]
    UnauthorizedGate,
    #[msg("Operation denied: The protocol is currently paused.")]
    ProtocolPaused,
    #[msg("Capacity reached: Maximum number of participants exceeded.")]
    MaxParticipantsReached,
    #[msg("Transaction denied: The resulting balance exceeds the maximum allowable debt limit.")]
    DebtLimitExceeded,
    #[msg("State corruption: Attempted to burn more supply than currently exists.")]
    SupplyUnderflow,
    #[msg("Parameter out of bounds: The provided edge weight exceeds the protocol limits.")]
    EdgeLimitExceeded,
    #[msg("Operation invalid: The protocol currently has zero registered participants.")]
    ZeroParticipants,
    #[msg("Physical limit reached: Applying negative entropy would overflow the system bounds.")]
    PhysicalOverflowLimit,
    #[msg("Exit denied: Participants cannot unregister while holding a negative balance (debt).")]
    DebtOnExitNotAllowed,
    #[msg("Mathematical error: An arithmetic operation resulted in an overflow or underflow.")]
    MathOverflow,
    #[msg("Unauthorized: Invalid authority for target user.")]
    UnauthorizedAuthority,
}

// ============================================================================
// EVENTS
// ============================================================================

#[event]
pub struct TransferEvent {
    pub from: Pubkey,
    pub to: Pubkey,
    pub amount: u128,
}

#[event]
pub struct RedistributeEvent {
    pub amount: u128,
    pub per_user: u128,
    pub dust_retained: u128,
}

#[event]
pub struct FieldUpdateEvent {
    pub new_global_field: i128,
}

#[event]
pub struct RegisteredEvent {
    pub user: Pubkey,
}

#[event]
pub struct UnregisteredEvent {
    pub user: Pubkey,
}

#[event]
pub struct BurnEvent {
    pub user: Pubkey,
    pub amount: u128,
}

#[event]
pub struct MintEvent {
    pub user: Pubkey,
    pub amount: u128,
}

#[event]
pub struct PausedEvent {
    pub paused: bool,
}
