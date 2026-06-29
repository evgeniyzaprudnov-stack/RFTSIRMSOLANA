use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount};
use rift_common::RiftError;
use ultra_core_rift::CoreState;

declare_id!("5yYh3k3nZs9q2xVhQ1uK7q9s7Jc8mG2hLs2sP3vR9b1A");

// ============================================================================
// CONSTANTS
// ============================================================================

pub const FOUNDER_SHARE_BPS: u16 = 314;  // 3.14% genesis allocation to admin vault
pub const MAX_FEE_BPS: u16 = 10;         // 0.10% maximum protocol fee

/// Floor for field_pressure in the mint-multiplier formula.
/// Prevents the multiplier from diverging when global_field is near zero.
/// At MIN_FIELD_PRESSURE the multiplier reaches its ceiling of 1e9.
pub const MIN_FIELD_PRESSURE: u128 = 1_000_000;

// ============================================================================
// ERRORS
// ============================================================================

#[error_code]
pub enum TokenError {
    #[msg("Fee exceeds maximum protocol limits.")]
    FeeTooHigh,
    #[msg("Invalid admin vault address.")]
    InvalidAdminVault,
    #[msg("Invalid core state address.")]
    InvalidCoreState,
    #[msg("Computed shares to mint is zero. Increase base_amount or wait for field pressure to decrease.")]
    ZeroSharesMinted,
}

// ============================================================================
// PROGRAM
// ============================================================================

#[program]
pub mod rift_token {
    use super::*;

    /// Initialize the token layer. Mints a genesis allocation to the admin vault.
    ///
    /// Security: called once by gate; seeds prevent re-initialization.
    pub fn initialize(
        ctx: Context<Initialize>,
        decimals: u8,
        fee_bps: u16,
        initial_supply: u64,
    ) -> Result<()> {
        require!(fee_bps <= MAX_FEE_BPS, TokenError::FeeTooHigh);

        let state = &mut ctx.accounts.rift_token_state;
        state.authority      = ctx.accounts.gate.key();
        state.core_state     = ctx.accounts.core_state.key();
        state.admin_vault    = ctx.accounts.admin_vault.key();
        state.decimals       = decimals;
        state.fee_bps        = fee_bps;
        state.total_shares   = 0;
        state.rift_multiplier = 1_000_000_000_000_000u128;
        state.bump           = ctx.bumps.rift_token_state;

        // founder_share = initial_supply * FOUNDER_SHARE_BPS / 10_000
        // intermediate is u128 to avoid overflow during multiplication;
        // result is <= initial_supply (u64), so the downcast is safe.
        let founder_share_u128 = (initial_supply as u128)
            .checked_mul(FOUNDER_SHARE_BPS as u128)
            .ok_or(RiftError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(RiftError::MathOverflow)?;

        let founder_share: u64 = founder_share_u128
            .try_into()
            .map_err(|_| RiftError::MathOverflow)?;

        if founder_share > 0 {
            let auth_bump = ctx.bumps.rift_authority;
            let signer_seeds: &[&[&[u8]]] = &[&[b"rift_mint_authority", &[auth_bump]]];

            let cpi_accounts = token::MintTo {
                mint:      ctx.accounts.rift_mint.to_account_info(),
                to:        ctx.accounts.admin_vault_token_account.to_account_info(),
                authority: ctx.accounts.rift_authority.to_account_info(),
            };
            token::mint_to(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    cpi_accounts,
                    signer_seeds,
                ),
                founder_share,
            )?;

            state.total_shares = state.total_shares
                .checked_add(founder_share)
                .ok_or(RiftError::MathOverflow)?;
        }

        Ok(())
    }

    /// Issue RIFT shares to a user in exchange for a SOL-denominated base_amount.
    ///
    /// Shares minted = (base_amount - fee) * (1e15 / field_pressure) / 1e12
    ///
    /// The mint multiplier is inversely proportional to field_pressure so that
    /// higher global_field → cheaper shares. field_pressure is floored at
    /// MIN_FIELD_PRESSURE to cap the multiplier.
    ///
    /// Security:
    ///   - core_state address verified against stored value (anti-substitution).
    ///   - admin_vault address verified against stored value.
    ///   - Protocol paused state checked explicitly (check_invariant does not cover it).
    ///   - shares_to_mint > 0 enforced so users cannot pay a fee and receive nothing.
    ///   - All u128 → u64 downcasts are checked.
    pub fn issue_rift(ctx: Context<IssueRift>, base_amount: u64) -> Result<()> {
        let core = &ctx.accounts.core_state;

        // Verify the economic invariant is intact before any mutation.
        core.check_invariant()?;

        // check_invariant does not inspect the paused flag.
        require!(!core.paused, RiftError::ProtocolPaused);

        let state = &mut ctx.accounts.rift_token_state;

        // fee_amount = base_amount * fee_bps / 10_000
        // base_amount is u64; intermediate is u128 to avoid overflow.
        // Result is <= base_amount, so the u64 downcast is always safe,
        // but we use try_into for explicit correctness.
        let fee_amount_u128 = (base_amount as u128)
            .checked_mul(state.fee_bps as u128)
            .ok_or(RiftError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(RiftError::MathOverflow)?;

        let fee_amount: u64 = fee_amount_u128
            .try_into()
            .map_err(|_| RiftError::MathOverflow)?;

        let amount_after_fee = base_amount
            .checked_sub(fee_amount)
            .ok_or(RiftError::MathOverflow)?;

        // field_pressure = max(|global_field|, MIN_FIELD_PRESSURE)
        // unsigned_abs() on i128 gives u128; max() floors it at MIN_FIELD_PRESSURE.
        let field_pressure = core.global_field
            .unsigned_abs()
            .max(MIN_FIELD_PRESSURE);

        // mint_multiplier = 1e15 / field_pressure
        // field_pressure >= MIN_FIELD_PRESSURE = 1e6, so mint_multiplier <= 1e9.
        // Division by zero is structurally impossible here; unwrap_or is defensive.
        let mint_multiplier = 1_000_000_000_000_000u128
            .checked_div(field_pressure)
            .unwrap_or(1_000_000_000_000u128);

        // shares_to_mint = amount_after_fee * mint_multiplier / 1e12
        // Max value: u64::MAX * 1e9 / 1e12 ≈ 1.84e16, which fits in u64.
        // The intermediate product (before /1e12) is at most ~1.84e28, which
        // fits in u128::MAX (~3.4e38). Both operations are safe.
        let shares_to_mint_u128 = (amount_after_fee as u128)
            .checked_mul(mint_multiplier)
            .ok_or(RiftError::MathOverflow)?
            .checked_div(1_000_000_000_000u128)
            .ok_or(RiftError::MathOverflow)?;

        // Prevent the pathological case where the user pays a fee but receives
        // zero tokens. SPL Token's mint_to silently accepts zero amounts.
        require!(shares_to_mint_u128 > 0, TokenError::ZeroSharesMinted);

        // Downcast to u64 required by SPL Token. Proven safe by the analysis
        // above (max shares_to_mint ≈ 1.84e16 << u64::MAX ≈ 1.84e19), but
        // we use try_into to catch any future formula changes.
        let shares_to_mint: u64 = shares_to_mint_u128
            .try_into()
            .map_err(|_| RiftError::MathOverflow)?;

        // Collect the protocol fee in SOL. The entire instruction is atomic:
        // if the subsequent mint CPI fails, this transfer is also rolled back.
        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.user.key(),
                &ctx.accounts.admin_vault.key(),
                fee_amount,
            ),
            &[
                ctx.accounts.user.to_account_info(),
                ctx.accounts.admin_vault.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;

        // Mint RIFT shares to the user.
        let auth_bump = ctx.bumps.rift_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[b"rift_mint_authority", &[auth_bump]]];

        token::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                token::MintTo {
                    mint:      ctx.accounts.rift_mint.to_account_info(),
                    to:        ctx.accounts.user_token_account.to_account_info(),
                    authority: ctx.accounts.rift_authority.to_account_info(),
                },
                signer_seeds,
            ),
            shares_to_mint,
        )?;

        state.total_shares = state.total_shares
            .checked_add(shares_to_mint)
            .ok_or(RiftError::MathOverflow)?;

        emit!(IssueRiftEvent {
            user:           ctx.accounts.user.key(),
            base_amount,
            fee_amount,
            shares_minted:  shares_to_mint,
            global_field:   core.global_field,
            rift_multiplier: state.rift_multiplier,
        });

        Ok(())
    }

    /// Gate-only: synchronise rift_multiplier with the current global_field.
    ///
    /// rift_multiplier is a cached view of the mint rate; calling rebase after
    /// a significant field change keeps it accurate for off-chain consumers.
    /// It does not affect the Core invariant.
    pub fn rebase(ctx: Context<Rebase>) -> Result<()> {
        let core  = &ctx.accounts.core_state;
        let state = &mut ctx.accounts.rift_token_state;

        core.check_invariant()?;

        let field_pressure = core.global_field
            .unsigned_abs()
            .max(MIN_FIELD_PRESSURE);

        let new_multiplier = 1_000_000_000_000_000u128
            .checked_div(field_pressure)
            .unwrap_or(1_000_000_000_000u128);

        let old_multiplier = state.rift_multiplier;
        state.rift_multiplier = new_multiplier;

        emit!(RiftRebaseEvent {
            old_multiplier,
            new_multiplier,
            global_field: core.global_field,
        });

        Ok(())
    }
}

// ============================================================================
// STATE
// ============================================================================

#[account]
pub struct RiftTokenState {
    pub authority:      Pubkey,  // 32 — gate that controls rebase
    pub core_state:     Pubkey,  // 32 — address of the bound CoreState
    pub admin_vault:    Pubkey,  // 32 — receives genesis share and SOL fees
    pub decimals:       u8,      //  1
    pub fee_bps:        u16,     //  2
    pub total_shares:   u64,     //  8
    pub rift_multiplier: u128,   // 16
    pub bump:           u8,      //  1
}
// On-chain size: 8 (discriminator) + 32*3 + 1 + 2 + 8 + 16 + 1 = 132 bytes

// ============================================================================
// INSTRUCTION CONTEXTS
// ============================================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = gate,
        space = 8 + 32 * 3 + 1 + 2 + 8 + 16 + 1, // = 132
        seeds = [b"rift_token_state"],
        bump
    )]
    pub rift_token_state: Account<'info, RiftTokenState>,

    /// The CoreState this token layer is bound to. Address stored at init.
    pub core_state: Account<'info, CoreState>,

    #[account(mut)]
    pub rift_mint: Account<'info, Mint>,

    #[account(mut)]
    pub admin_vault_token_account: Account<'info, TokenAccount>,

    /// CHECK: Admin vault. Receives the genesis share (SPL) and protocol fees
    /// (SOL). No ownership constraint required; address recorded in state.
    pub admin_vault: UncheckedAccount<'info>,

    /// CHECK: PDA that acts as the SPL mint authority. Validated by seeds/bump.
    #[account(seeds = [b"rift_mint_authority"], bump)]
    pub rift_authority: UncheckedAccount<'info>,

    #[account(mut)]
    pub gate: Signer<'info>,

    pub token_program:  Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct IssueRift<'info> {
    #[account(mut)]
    pub rift_token_state: Account<'info, RiftTokenState>,

    /// CoreState must match the address stored at initialization.
    /// This prevents an attacker from supplying a manipulated CoreState.
    #[account(
        constraint = core_state.key() == rift_token_state.core_state
            @ TokenError::InvalidCoreState
    )]
    pub core_state: Account<'info, CoreState>,

    #[account(mut)]
    pub rift_mint: Account<'info, Mint>,

    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,

    /// CHECK: PDA mint authority. Verified by seeds; signed in CPI.
    #[account(seeds = [b"rift_mint_authority"], bump)]
    pub rift_authority: UncheckedAccount<'info>,

    #[account(mut)]
    pub user: Signer<'info>,

    /// CHECK: SOL fee recipient. Address verified against stored value.
    #[account(
        mut,
        constraint = admin_vault.key() == rift_token_state.admin_vault
            @ TokenError::InvalidAdminVault
    )]
    pub admin_vault: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub token_program:  Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Rebase<'info> {
    /// Only the authority stored at initialization (the gate) may call rebase.
    #[account(
        mut,
        has_one = authority @ RiftError::UnauthorizedGate
    )]
    pub rift_token_state: Account<'info, RiftTokenState>,

    /// CoreState must match the address stored at initialization.
    #[account(
        constraint = core_state.key() == rift_token_state.core_state
            @ TokenError::InvalidCoreState
    )]
    pub core_state: Account<'info, CoreState>,

    pub authority: Signer<'info>,
}

// ============================================================================
// EVENTS
// ============================================================================

#[event]
pub struct IssueRiftEvent {
    pub user:            Pubkey,
    pub base_amount:     u64,
    pub fee_amount:      u64,
    pub shares_minted:   u64,
    pub global_field:    i128,
    pub rift_multiplier: u128,
}

#[event]
pub struct RiftRebaseEvent {
    pub old_multiplier: u128,
    pub new_multiplier: u128,
    pub global_field:   i128,
}
