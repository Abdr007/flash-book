use std::mem::size_of;

pub type DataIndex = u32;

/// Marker trait to emit warnings when using get_helper on the Value type
/// rather than on Node<Value>
pub trait Get: bytemuck::Pod {}

/// Read a struct of type T in an array of data at a given index.
pub fn get_helper<T: Get>(data: &[u8], index: DataIndex) -> &T {
    let index_usize: usize = index as usize;
    bytemuck::from_bytes(&data[index_usize..index_usize + size_of::<T>()])
}

/// Read a struct of type T in an array of data at a given index.
pub fn get_mut_helper<T: Get>(data: &mut [u8], index: DataIndex) -> &mut T {
    let index_usize: usize = index as usize;
    bytemuck::from_bytes_mut(&mut data[index_usize..index_usize + size_of::<T>()])
}

/// Bounds-checked read — returns `None` instead of panicking when the
/// index + struct size would run off the end of `data` (e.g. a malformed /
/// short account, or an out-of-range index supplied by an untrusted caller
/// such as the sequencer). Solana programs must not panic on attacker input;
/// use this at externally-reachable entry points where the `DataIndex` is not
/// already proven in-bounds by the allocator / tree structure.
pub fn get_helper_checked<T: Get>(data: &[u8], index: DataIndex) -> Option<&T> {
    let start = index as usize;
    let end = start.checked_add(size_of::<T>())?;
    let slice = data.get(start..end)?;
    Some(bytemuck::from_bytes(slice))
}

/// Bounds-checked, in-range predicate for an index into the slab.
/// True iff a `T` at `index` lies fully within `data`.
pub fn index_in_bounds<T: Get>(data: &[u8], index: DataIndex) -> bool {
    (index as usize)
        .checked_add(size_of::<T>())
        .is_some_and(|end| end <= data.len())
}

/// The standard `bool` is not a `Pod`, define a replacement that is
/// https://docs.rs/spl-pod/latest/src/spl_pod/primitives.rs.html#13
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(transparent)]
pub struct PodBool(pub u8);
impl PodBool {
    pub const fn from_bool(b: bool) -> Self {
        Self(if b { 1 } else { 0 })
    }
}

impl From<bool> for PodBool {
    fn from(b: bool) -> Self {
        Self::from_bool(b)
    }
}

#[test]
fn test_pod_bool() {
    assert!(PodBool::from_bool(false).0 != 1);
    assert!(PodBool::from(false).0 != 1);
}

#[test]
fn checked_helpers_reject_out_of_bounds() {
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    #[repr(C)]
    struct Four([u8; 4]);
    impl Get for Four {}
    let data = [0u8; 8];
    // In-bounds reads succeed.
    assert!(get_helper_checked::<Four>(&data, 0).is_some());
    assert!(get_helper_checked::<Four>(&data, 4).is_some());
    assert!(index_in_bounds::<Four>(&data, 4));
    // Out-of-bounds (would panic via raw `get_helper`) returns None / false.
    assert!(get_helper_checked::<Four>(&data, 5).is_none()); // 5+4=9 > 8
    assert!(!index_in_bounds::<Four>(&data, 5));
    // Overflow-safe at the extreme index.
    assert!(get_helper_checked::<Four>(&data, u32::MAX).is_none());
    assert!(!index_in_bounds::<Four>(&data, u32::MAX));
}

#[macro_export]
#[cfg(not(feature = "certora"))]
macro_rules! trace {
    ($($arg:tt)*) => {
        #[cfg(feature = "trace")]
        {
            #[cfg(target_os = "solana")]
            {
            solana_program::msg!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            }
            #[cfg(not(target_os = "solana"))]
            {
            std::println!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            }
        }
    };
}

#[macro_export]
#[cfg(feature = "certora")]
macro_rules! trace {
    ($($arg:tt)*) => {};
}
