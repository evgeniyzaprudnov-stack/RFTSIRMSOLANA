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

