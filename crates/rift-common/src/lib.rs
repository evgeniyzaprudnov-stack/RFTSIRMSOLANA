use anchor_lang::prelude::*;

pub const NEG_E: i128 = -2_718_281_828_459_045_235;
pub const MAX_PARTICIPANTS: u64 = 1_000_000_000_000;
pub const MAX_EDGE_COST: i128 = 1_000_000_000_000_000_000_000;
pub const MIN_ABS_DEBT: i128 = -1_000_000_000_000_000_000;
pub const MAX_SUPPLY: u128 = i128::MAX as u128;
pub const NEG_E_MAX_P: i128 = i128::MAX / (-NEG_E);

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
    #[msg("Exit denied: Participants cannot unregister while holding a negative balance (debt)." )]
    DebtOnExitNotAllowed,
    #[msg("Mathematical error: An arithmetic operation resulted in an overflow or underflow.")]
    MathOverflow,
    #[msg("Unauthorized: Invalid authority for target user.")]
    UnauthorizedAuthority,
}

#[account]
pub struct UserAccount {
    pub authority: Pubkey,
    pub base_balance: i128,
}

impl UserAccount {
    pub const SPACE: usize = 8 + 32 + 16;
}

#[account]
pub struct EdgeAccount {
    pub weight: i128,
}

impl EdgeAccount {
    pub const SPACE: usize = 8 + 16;
}

#[account]
pub struct CoreState {
    pub gate: Pubkey,
    pub paused: bool,
    pub global_field: i128,
    pub total_base_sum: i128,
    pub total_supply: u128,
    pub total_minted: u128,
    pub total_burned: u128,
    pub p: u64,
    pub dust_accumulator: u128,
}

impl CoreState {
    pub const SPACE: usize = 8 + 32 + 1 + 16 * 6 + 8;

    pub fn debt_limit(&self) -> Result<i128> {
        let factor = (self.p as i128)
            .checked_mul(10)
            .ok_or(RiftError::MathOverflow)?;

        if factor == 0 {
            return Ok(MIN_ABS_DEBT);
        }

        let limit = (self.total_supply as i128)
            .checked_div(factor)
            .ok_or(RiftError::MathOverflow)?;
        Ok(-limit)
    }

    pub fn check_invariant(&self) -> Result<()> {
        require!(self.total_supply <= MAX_SUPPLY, RiftError::MathOverflow);

        let field_contrib = self.global_field
            .checked_mul(self.p as i128)
            .ok_or(RiftError::MathOverflow)?;

        let expected = self.total_base_sum
            .checked_add(field_contrib)
            .ok_or(RiftError::MathOverflow)?;

        let supply_signed = self.total_supply as i128;
        require!(supply_signed == expected, RiftError::InvariantViolation);

        require!(self.total_minted >= self.total_burned, RiftError::InvariantViolation);
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
