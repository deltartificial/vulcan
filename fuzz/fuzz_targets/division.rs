#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan::division::Divide;

// Compares `divide_floor`, `remainder` and `divide_exact` with reference arithmetic in a wider
// type, `u128`, where the plain operators cannot overflow and truncation is the floor for unsigned
// values. The input gives a dividend and a divisor, whose low bits feed every width; a zero divisor
// is skipped (it is a programmer error, tested by the unit tests), and `divide_exact` runs only
// when the reference leaves no remainder.
fuzz_target!(|data: &[u8]| {
    let Some(dividend_bytes) = data.get(..16) else { return };
    let Some(divisor_bytes) = data.get(16..32) else { return };
    let (Ok(dividend), Ok(divisor)) = (dividend_bytes.try_into(), divisor_bytes.try_into()) else {
        unreachable!("both ranges are 16 bytes long")
    };
    let operands =
        Operands { dividend: u128::from_le_bytes(dividend), divisor: u128::from_le_bytes(divisor) };
    check_width::<u8>(&operands);
    check_width::<u16>(&operands);
    check_width::<u32>(&operands);
    check_width::<u64>(&operands);
});

/// A dividend and a divisor, of which each width takes the low bits.
struct Operands {
    /// The value divided.
    dividend: u128,
    /// The value divided by.
    divisor: u128,
}

/// Compares the operations on `T` with the plain operators on `u128`.
fn check_width<T>(operands: &Operands)
where
    T: Divide + TryFrom<u128> + Into<u128>,
{
    let mask = u128::from(u64::MAX) >> (64 - 8 * size_of::<T>());
    let Ok(dividend) = T::try_from(operands.dividend & mask) else { unreachable!("masked") };
    let Ok(divisor) = T::try_from(operands.divisor & mask) else { unreachable!("masked") };
    let dividend_wide: u128 = dividend.into();
    let divisor_wide: u128 = divisor.into();
    if divisor_wide == 0 {
        return;
    }
    let quotient: u128 = dividend.divide_floor(divisor).into();
    let remainder: u128 = dividend.remainder(divisor).into();
    assert_eq!(quotient, dividend_wide / divisor_wide, "the floor matches truncation");
    assert_eq!(remainder, dividend_wide % divisor_wide, "the remainder matches the reference");
    assert_eq!(quotient * divisor_wide + remainder, dividend_wide, "the parts add back up");
    if remainder == 0 {
        let exact: u128 = dividend.divide_exact(divisor).into();
        assert_eq!(exact, quotient, "an exact division equals the floor");
    }
}
