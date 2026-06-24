//! Shared error type + checked-arithmetic helpers for the `no_std` core.
//!
//! Ported from the Anchor program's `errors.rs` (`FlashBookError` + the
//! `OrOverflow` trait). Pure-math modules return `Result<T> = Result<T,
//! FlashBookError>`; instruction handlers map `FlashBookError` to
//! `ProgramError` at the boundary.
//!
//! NOTE: `book.rs` still carries its own local `errors` mod (4 variants) from an
//! earlier port — consolidating it onto this module is a follow-up cleanup.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashBookError {
    ArithmeticOverflow,
    ArithmeticUnderflow,
    DivisionByZero,
    OutOfRange,
}

pub type Result<T> = core::result::Result<T, FlashBookError>;

/// `Option` → `Result` with a semantic error code (mirrors the Anchor trait).
pub trait OrOverflow<T> {
    fn or_overflow(self) -> Result<T>;
    fn or_underflow(self) -> Result<T>;
    fn or_div_zero(self) -> Result<T>;
}

impl<T> OrOverflow<T> for Option<T> {
    #[inline]
    fn or_overflow(self) -> Result<T> {
        self.ok_or(FlashBookError::ArithmeticOverflow)
    }
    #[inline]
    fn or_underflow(self) -> Result<T> {
        self.ok_or(FlashBookError::ArithmeticUnderflow)
    }
    #[inline]
    fn or_div_zero(self) -> Result<T> {
        self.ok_or(FlashBookError::DivisionByZero)
    }
}
