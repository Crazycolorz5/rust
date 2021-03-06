// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![unstable(feature = "allocator_api",
            reason = "the precise API and guarantees it provides may be tweaked \
                      slightly, especially to possibly take into account the \
                      types being stored to make room for a future \
                      tracing garbage collector",
            issue = "32838")]

use cmp;
use fmt;
use mem;
use usize;
use ptr::{self, NonNull};

extern {
    /// An opaque, unsized type. Used for pointers to allocated memory.
    ///
    /// This type can only be used behind a pointer like `*mut Opaque` or `ptr::NonNull<Opaque>`.
    /// Such pointers are similar to C’s `void*` type.
    pub type Opaque;
}

impl Opaque {
    /// Similar to `std::ptr::null`, which requires `T: Sized`.
    pub fn null() -> *const Self {
        0 as _
    }

    /// Similar to `std::ptr::null_mut`, which requires `T: Sized`.
    pub fn null_mut() -> *mut Self {
        0 as _
    }
}

/// Represents the combination of a starting address and
/// a total capacity of the returned block.
#[derive(Debug)]
pub struct Excess(pub NonNull<Opaque>, pub usize);

fn size_align<T>() -> (usize, usize) {
    (mem::size_of::<T>(), mem::align_of::<T>())
}

/// Layout of a block of memory.
///
/// An instance of `Layout` describes a particular layout of memory.
/// You build a `Layout` up as an input to give to an allocator.
///
/// All layouts have an associated non-negative size and a
/// power-of-two alignment.
///
/// (Note however that layouts are *not* required to have positive
/// size, even though many allocators require that all memory
/// requests have positive size. A caller to the `Alloc::alloc`
/// method must either ensure that conditions like this are met, or
/// use specific allocators with looser requirements.)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    // size of the requested block of memory, measured in bytes.
    size: usize,

    // alignment of the requested block of memory, measured in bytes.
    // we ensure that this is always a power-of-two, because API's
    // like `posix_memalign` require it and it is a reasonable
    // constraint to impose on Layout constructors.
    //
    // (However, we do not analogously require `align >= sizeof(void*)`,
    //  even though that is *also* a requirement of `posix_memalign`.)
    align: usize,
}


// FIXME: audit default implementations for overflow errors,
// (potentially switching to overflowing_add and
//  overflowing_mul as necessary).

impl Layout {
    /// Constructs a `Layout` from a given `size` and `align`,
    /// or returns `None` if either of the following conditions
    /// are not met:
    ///
    /// * `align` must be a power of two,
    ///
    /// * `size`, when rounded up to the nearest multiple of `align`,
    ///    must not overflow (i.e. the rounded value must be less than
    ///    `usize::MAX`).
    #[inline]
    pub fn from_size_align(size: usize, align: usize) -> Result<Self, LayoutErr> {
        if !align.is_power_of_two() {
            return Err(LayoutErr { private: () });
        }

        // (power-of-two implies align != 0.)

        // Rounded up size is:
        //   size_rounded_up = (size + align - 1) & !(align - 1);
        //
        // We know from above that align != 0. If adding (align - 1)
        // does not overflow, then rounding up will be fine.
        //
        // Conversely, &-masking with !(align - 1) will subtract off
        // only low-order-bits. Thus if overflow occurs with the sum,
        // the &-mask cannot subtract enough to undo that overflow.
        //
        // Above implies that checking for summation overflow is both
        // necessary and sufficient.
        if size > usize::MAX - (align - 1) {
            return Err(LayoutErr { private: () });
        }

        unsafe {
            Ok(Layout::from_size_align_unchecked(size, align))
        }
    }

    /// Creates a layout, bypassing all checks.
    ///
    /// # Safety
    ///
    /// This function is unsafe as it does not verify that `align` is
    /// a power-of-two nor `size` aligned to `align` fits within the
    /// address space (i.e. the `Layout::from_size_align` preconditions).
    #[inline]
    pub unsafe fn from_size_align_unchecked(size: usize, align: usize) -> Self {
        Layout { size: size, align: align }
    }

    /// The minimum size in bytes for a memory block of this layout.
    #[inline]
    pub fn size(&self) -> usize { self.size }

    /// The minimum byte alignment for a memory block of this layout.
    #[inline]
    pub fn align(&self) -> usize { self.align }

    /// Constructs a `Layout` suitable for holding a value of type `T`.
    pub fn new<T>() -> Self {
        let (size, align) = size_align::<T>();
        // Note that the align is guaranteed by rustc to be a power of two and
        // the size+align combo is guaranteed to fit in our address space. As a
        // result use the unchecked constructor here to avoid inserting code
        // that panics if it isn't optimized well enough.
        debug_assert!(Layout::from_size_align(size, align).is_ok());
        unsafe {
            Layout::from_size_align_unchecked(size, align)
        }
    }

    /// Produces layout describing a record that could be used to
    /// allocate backing structure for `T` (which could be a trait
    /// or other unsized type like a slice).
    pub fn for_value<T: ?Sized>(t: &T) -> Self {
        let (size, align) = (mem::size_of_val(t), mem::align_of_val(t));
        // See rationale in `new` for why this us using an unsafe variant below
        debug_assert!(Layout::from_size_align(size, align).is_ok());
        unsafe {
            Layout::from_size_align_unchecked(size, align)
        }
    }

    /// Creates a layout describing the record that can hold a value
    /// of the same layout as `self`, but that also is aligned to
    /// alignment `align` (measured in bytes).
    ///
    /// If `self` already meets the prescribed alignment, then returns
    /// `self`.
    ///
    /// Note that this method does not add any padding to the overall
    /// size, regardless of whether the returned layout has a different
    /// alignment. In other words, if `K` has size 16, `K.align_to(32)`
    /// will *still* have size 16.
    ///
    /// # Panics
    ///
    /// Panics if the combination of `self.size` and the given `align`
    /// violates the conditions listed in `from_size_align`.
    #[inline]
    pub fn align_to(&self, align: usize) -> Self {
        Layout::from_size_align(self.size, cmp::max(self.align, align)).unwrap()
    }

    /// Returns the amount of padding we must insert after `self`
    /// to ensure that the following address will satisfy `align`
    /// (measured in bytes).
    ///
    /// E.g. if `self.size` is 9, then `self.padding_needed_for(4)`
    /// returns 3, because that is the minimum number of bytes of
    /// padding required to get a 4-aligned address (assuming that the
    /// corresponding memory block starts at a 4-aligned address).
    ///
    /// The return value of this function has no meaning if `align` is
    /// not a power-of-two.
    ///
    /// Note that the utility of the returned value requires `align`
    /// to be less than or equal to the alignment of the starting
    /// address for the whole allocated block of memory. One way to
    /// satisfy this constraint is to ensure `align <= self.align`.
    #[inline]
    pub fn padding_needed_for(&self, align: usize) -> usize {
        let len = self.size();

        // Rounded up value is:
        //   len_rounded_up = (len + align - 1) & !(align - 1);
        // and then we return the padding difference: `len_rounded_up - len`.
        //
        // We use modular arithmetic throughout:
        //
        // 1. align is guaranteed to be > 0, so align - 1 is always
        //    valid.
        //
        // 2. `len + align - 1` can overflow by at most `align - 1`,
        //    so the &-mask wth `!(align - 1)` will ensure that in the
        //    case of overflow, `len_rounded_up` will itself be 0.
        //    Thus the returned padding, when added to `len`, yields 0,
        //    which trivially satisfies the alignment `align`.
        //
        // (Of course, attempts to allocate blocks of memory whose
        // size and padding overflow in the above manner should cause
        // the allocator to yield an error anyway.)

        let len_rounded_up = len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1);
        return len_rounded_up.wrapping_sub(len);
    }

    /// Creates a layout describing the record for `n` instances of
    /// `self`, with a suitable amount of padding between each to
    /// ensure that each instance is given its requested size and
    /// alignment. On success, returns `(k, offs)` where `k` is the
    /// layout of the array and `offs` is the distance between the start
    /// of each element in the array.
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn repeat(&self, n: usize) -> Result<(Self, usize), LayoutErr> {
        let padded_size = self.size.checked_add(self.padding_needed_for(self.align))
            .ok_or(LayoutErr { private: () })?;
        let alloc_size = padded_size.checked_mul(n)
            .ok_or(LayoutErr { private: () })?;
        Ok((Layout::from_size_align(alloc_size, self.align)?, padded_size))
    }

    /// Creates a layout describing the record for `self` followed by
    /// `next`, including any necessary padding to ensure that `next`
    /// will be properly aligned. Note that the result layout will
    /// satisfy the alignment properties of both `self` and `next`.
    ///
    /// Returns `Some((k, offset))`, where `k` is layout of the concatenated
    /// record and `offset` is the relative location, in bytes, of the
    /// start of the `next` embedded within the concatenated record
    /// (assuming that the record itself starts at offset 0).
    ///
    /// On arithmetic overflow, returns `None`.
    pub fn extend(&self, next: Self) -> Result<(Self, usize), LayoutErr> {
        let new_align = cmp::max(self.align, next.align);
        let realigned = Layout::from_size_align(self.size, new_align)?;

        let pad = realigned.padding_needed_for(next.align);

        let offset = self.size.checked_add(pad)
            .ok_or(LayoutErr { private: () })?;
        let new_size = offset.checked_add(next.size)
            .ok_or(LayoutErr { private: () })?;

        let layout = Layout::from_size_align(new_size, new_align)?;
        Ok((layout, offset))
    }

    /// Creates a layout describing the record for `n` instances of
    /// `self`, with no padding between each instance.
    ///
    /// Note that, unlike `repeat`, `repeat_packed` does not guarantee
    /// that the repeated instances of `self` will be properly
    /// aligned, even if a given instance of `self` is properly
    /// aligned. In other words, if the layout returned by
    /// `repeat_packed` is used to allocate an array, it is not
    /// guaranteed that all elements in the array will be properly
    /// aligned.
    ///
    /// On arithmetic overflow, returns `None`.
    pub fn repeat_packed(&self, n: usize) -> Result<Self, LayoutErr> {
        let size = self.size().checked_mul(n).ok_or(LayoutErr { private: () })?;
        Layout::from_size_align(size, self.align)
    }

    /// Creates a layout describing the record for `self` followed by
    /// `next` with no additional padding between the two. Since no
    /// padding is inserted, the alignment of `next` is irrelevant,
    /// and is not incorporated *at all* into the resulting layout.
    ///
    /// Returns `(k, offset)`, where `k` is layout of the concatenated
    /// record and `offset` is the relative location, in bytes, of the
    /// start of the `next` embedded within the concatenated record
    /// (assuming that the record itself starts at offset 0).
    ///
    /// (The `offset` is always the same as `self.size()`; we use this
    ///  signature out of convenience in matching the signature of
    ///  `extend`.)
    ///
    /// On arithmetic overflow, returns `None`.
    pub fn extend_packed(&self, next: Self) -> Result<(Self, usize), LayoutErr> {
        let new_size = self.size().checked_add(next.size())
            .ok_or(LayoutErr { private: () })?;
        let layout = Layout::from_size_align(new_size, self.align)?;
        Ok((layout, self.size()))
    }

    /// Creates a layout describing the record for a `[T; n]`.
    ///
    /// On arithmetic overflow, returns `None`.
    pub fn array<T>(n: usize) -> Result<Self, LayoutErr> {
        Layout::new::<T>()
            .repeat(n)
            .map(|(k, offs)| {
                debug_assert!(offs == mem::size_of::<T>());
                k
            })
    }
}

/// The parameters given to `Layout::from_size_align` do not satisfy
/// its documented constraints.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutErr {
    private: ()
}

// (we need this for downstream impl of trait Error)
impl fmt::Display for LayoutErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("invalid parameters to Layout::from_size_align")
    }
}

/// The `AllocErr` error specifies whether an allocation failure is
/// specifically due to resource exhaustion or if it is due to
/// something wrong when combining the given input arguments with this
/// allocator.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AllocErr;

// (we need this for downstream impl of trait Error)
impl fmt::Display for AllocErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("memory allocation failed")
    }
}

/// The `CannotReallocInPlace` error is used when `grow_in_place` or
/// `shrink_in_place` were unable to reuse the given memory block for
/// a requested layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CannotReallocInPlace;

impl CannotReallocInPlace {
    pub fn description(&self) -> &str {
        "cannot reallocate allocator's memory in place"
    }
}

// (we need this for downstream impl of trait Error)
impl fmt::Display for CannotReallocInPlace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// Augments `AllocErr` with a CapacityOverflow variant.
#[derive(Clone, PartialEq, Eq, Debug)]
#[unstable(feature = "try_reserve", reason = "new API", issue="48043")]
pub enum CollectionAllocErr {
    /// Error due to the computed capacity exceeding the collection's maximum
    /// (usually `isize::MAX` bytes).
    CapacityOverflow,
    /// Error due to the allocator (see the `AllocErr` type's docs).
    AllocErr,
}

#[unstable(feature = "try_reserve", reason = "new API", issue="48043")]
impl From<AllocErr> for CollectionAllocErr {
    #[inline]
    fn from(AllocErr: AllocErr) -> Self {
        CollectionAllocErr::AllocErr
    }
}

#[unstable(feature = "try_reserve", reason = "new API", issue="48043")]
impl From<LayoutErr> for CollectionAllocErr {
    #[inline]
    fn from(_: LayoutErr) -> Self {
        CollectionAllocErr::CapacityOverflow
    }
}

/// A memory allocator that can be registered to be the one backing `std::alloc::Global`
/// though the `#[global_allocator]` attributes.
pub unsafe trait GlobalAlloc {
    /// Allocate memory as described by the given `layout`.
    ///
    /// Returns a pointer to newly-allocated memory,
    /// or NULL to indicate allocation failure.
    ///
    /// # Safety
    ///
    /// **FIXME:** what are the exact requirements?
    unsafe fn alloc(&self, layout: Layout) -> *mut Opaque;

    /// Deallocate the block of memory at the given `ptr` pointer with the given `layout`.
    ///
    /// # Safety
    ///
    /// **FIXME:** what are the exact requirements?
    /// In particular around layout *fit*. (See docs for the `Alloc` trait.)
    unsafe fn dealloc(&self, ptr: *mut Opaque, layout: Layout);

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut Opaque {
        let size = layout.size();
        let ptr = self.alloc(layout);
        if !ptr.is_null() {
            ptr::write_bytes(ptr as *mut u8, 0, size);
        }
        ptr
    }

    /// Shink or grow a block of memory to the given `new_size`.
    /// The block is described by the given `ptr` pointer and `layout`.
    ///
    /// Return a new pointer (which may or may not be the same as `ptr`),
    /// or NULL to indicate reallocation failure.
    ///
    /// If reallocation is successful, the old `ptr` pointer is considered
    /// to have been deallocated.
    ///
    /// # Safety
    ///
    /// `new_size`, when rounded up to the nearest multiple of `old_layout.align()`,
    /// must not overflow (i.e. the rounded value must be less than `usize::MAX`).
    ///
    /// **FIXME:** what are the exact requirements?
    /// In particular around layout *fit*. (See docs for the `Alloc` trait.)
    unsafe fn realloc(&self, ptr: *mut Opaque, layout: Layout, new_size: usize) -> *mut Opaque {
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let new_ptr = self.alloc(new_layout);
        if !new_ptr.is_null() {
            ptr::copy_nonoverlapping(
                ptr as *const u8,
                new_ptr as *mut u8,
                cmp::min(layout.size(), new_size),
            );
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}

/// An implementation of `Alloc` can allocate, reallocate, and
/// deallocate arbitrary blocks of data described via `Layout`.
///
/// Some of the methods require that a memory block be *currently
/// allocated* via an allocator. This means that:
///
/// * the starting address for that memory block was previously
///   returned by a previous call to an allocation method (`alloc`,
///   `alloc_zeroed`, `alloc_excess`, `alloc_one`, `alloc_array`) or
///   reallocation method (`realloc`, `realloc_excess`, or
///   `realloc_array`), and
///
/// * the memory block has not been subsequently deallocated, where
///   blocks are deallocated either by being passed to a deallocation
///   method (`dealloc`, `dealloc_one`, `dealloc_array`) or by being
///   passed to a reallocation method (see above) that returns `Ok`.
///
/// A note regarding zero-sized types and zero-sized layouts: many
/// methods in the `Alloc` trait state that allocation requests
/// must be non-zero size, or else undefined behavior can result.
///
/// * However, some higher-level allocation methods (`alloc_one`,
///   `alloc_array`) are well-defined on zero-sized types and can
///   optionally support them: it is left up to the implementor
///   whether to return `Err`, or to return `Ok` with some pointer.
///
/// * If an `Alloc` implementation chooses to return `Ok` in this
///   case (i.e. the pointer denotes a zero-sized inaccessible block)
///   then that returned pointer must be considered "currently
///   allocated". On such an allocator, *all* methods that take
///   currently-allocated pointers as inputs must accept these
///   zero-sized pointers, *without* causing undefined behavior.
///
/// * In other words, if a zero-sized pointer can flow out of an
///   allocator, then that allocator must likewise accept that pointer
///   flowing back into its deallocation and reallocation methods.
///
/// Some of the methods require that a layout *fit* a memory block.
/// What it means for a layout to "fit" a memory block means (or
/// equivalently, for a memory block to "fit" a layout) is that the
/// following two conditions must hold:
///
/// 1. The block's starting address must be aligned to `layout.align()`.
///
/// 2. The block's size must fall in the range `[use_min, use_max]`, where:
///
///    * `use_min` is `self.usable_size(layout).0`, and
///
///    * `use_max` is the capacity that was (or would have been)
///      returned when (if) the block was allocated via a call to
///      `alloc_excess` or `realloc_excess`.
///
/// Note that:
///
///  * the size of the layout most recently used to allocate the block
///    is guaranteed to be in the range `[use_min, use_max]`, and
///
///  * a lower-bound on `use_max` can be safely approximated by a call to
///    `usable_size`.
///
///  * if a layout `k` fits a memory block (denoted by `ptr`)
///    currently allocated via an allocator `a`, then it is legal to
///    use that layout to deallocate it, i.e. `a.dealloc(ptr, k);`.
///
/// # Unsafety
///
/// The `Alloc` trait is an `unsafe` trait for a number of reasons, and
/// implementors must ensure that they adhere to these contracts:
///
/// * Pointers returned from allocation functions must point to valid memory and
///   retain their validity until at least the instance of `Alloc` is dropped
///   itself.
///
/// * It's undefined behavior if global allocators unwind.  This restriction may
///   be lifted in the future, but currently a panic from any of these
///   functions may lead to memory unsafety. Note that as of the time of this
///   writing allocators *not* intending to be global allocators can still panic
///   in their implementation without violating memory safety.
///
/// * `Layout` queries and calculations in general must be correct. Callers of
///   this trait are allowed to rely on the contracts defined on each method,
///   and implementors must ensure such contracts remain true.
///
/// Note that this list may get tweaked over time as clarifications are made in
/// the future. Additionally global allocators may gain unique requirements for
/// how to safely implement one in the future as well.
pub unsafe trait Alloc {

    // (Note: existing allocators have unspecified but well-defined
    // behavior in response to a zero size allocation request ;
    // e.g. in C, `malloc` of 0 will either return a null pointer or a
    // unique pointer, but will not have arbitrary undefined
    // behavior. Rust should consider revising the alloc::heap crate
    // to reflect this reality.)

    /// Returns a pointer meeting the size and alignment guarantees of
    /// `layout`.
    ///
    /// If this method returns an `Ok(addr)`, then the `addr` returned
    /// will be non-null address pointing to a block of storage
    /// suitable for holding an instance of `layout`.
    ///
    /// The returned block of storage may or may not have its contents
    /// initialized. (Extension subtraits might restrict this
    /// behavior, e.g. to ensure initialization to particular sets of
    /// bit patterns.)
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure that `layout` has non-zero size.
    ///
    /// (Extension subtraits might provide more specific bounds on
    /// behavior, e.g. guarantee a sentinel address or a null pointer
    /// in response to a zero-size allocation request.)
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// Implementations are encouraged to return `Err` on memory
    /// exhaustion rather than panicking or aborting, but this is not
    /// a strict requirement. (Specifically: it is *legal* to
    /// implement this trait atop an underlying native allocation
    /// library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn alloc(&mut self, layout: Layout) -> Result<NonNull<Opaque>, AllocErr>;

    /// Deallocate the memory referenced by `ptr`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must denote a block of memory currently allocated via
    ///   this allocator,
    ///
    /// * `layout` must *fit* that block of memory,
    ///
    /// * In addition to fitting the block of memory `layout`, the
    ///   alignment of the `layout` must match the alignment used
    ///   to allocate that block of memory.
    unsafe fn dealloc(&mut self, ptr: NonNull<Opaque>, layout: Layout);

    // == ALLOCATOR-SPECIFIC QUANTITIES AND LIMITS ==
    // usable_size

    /// Returns bounds on the guaranteed usable size of a successful
    /// allocation created with the specified `layout`.
    ///
    /// In particular, if one has a memory block allocated via a given
    /// allocator `a` and layout `k` where `a.usable_size(k)` returns
    /// `(l, u)`, then one can pass that block to `a.dealloc()` with a
    /// layout in the size range [l, u].
    ///
    /// (All implementors of `usable_size` must ensure that
    /// `l <= k.size() <= u`)
    ///
    /// Both the lower- and upper-bounds (`l` and `u` respectively)
    /// are provided, because an allocator based on size classes could
    /// misbehave if one attempts to deallocate a block without
    /// providing a correct value for its size (i.e., one within the
    /// range `[l, u]`).
    ///
    /// Clients who wish to make use of excess capacity are encouraged
    /// to use the `alloc_excess` and `realloc_excess` instead, as
    /// this method is constrained to report conservative values that
    /// serve as valid bounds for *all possible* allocation method
    /// calls.
    ///
    /// However, for clients that do not wish to track the capacity
    /// returned by `alloc_excess` locally, this method is likely to
    /// produce useful results.
    #[inline]
    fn usable_size(&self, layout: &Layout) -> (usize, usize) {
        (layout.size(), layout.size())
    }

    // == METHODS FOR MEMORY REUSE ==
    // realloc. alloc_excess, realloc_excess

    /// Returns a pointer suitable for holding data described by
    /// a new layout with `layout`’s alginment and a size given
    /// by `new_size`. To
    /// accomplish this, this may extend or shrink the allocation
    /// referenced by `ptr` to fit the new layout.
    ///
    /// If this returns `Ok`, then ownership of the memory block
    /// referenced by `ptr` has been transferred to this
    /// allocator. The memory may or may not have been freed, and
    /// should be considered unusable (unless of course it was
    /// transferred back to the caller again via the return value of
    /// this method).
    ///
    /// If this method returns `Err`, then ownership of the memory
    /// block has not been transferred to this allocator, and the
    /// contents of the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * `layout` must *fit* the `ptr` (see above). (The `new_size`
    ///   argument need not fit it.)
    ///
    /// * `new_size` must be greater than zero.
    ///
    /// * `new_size`, when rounded up to the nearest multiple of `layout.align()`,
    ///   must not overflow (i.e. the rounded value must be less than `usize::MAX`).
    ///
    /// (Extension subtraits might provide more specific bounds on
    /// behavior, e.g. guarantee a sentinel address or a null pointer
    /// in response to a zero-size allocation request.)
    ///
    /// # Errors
    ///
    /// Returns `Err` only if the new layout
    /// does not meet the allocator's size
    /// and alignment constraints of the allocator, or if reallocation
    /// otherwise fails.
    ///
    /// Implementations are encouraged to return `Err` on memory
    /// exhaustion rather than panicking or aborting, but this is not
    /// a strict requirement. (Specifically: it is *legal* to
    /// implement this trait atop an underlying native allocation
    /// library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn realloc(&mut self,
                      ptr: NonNull<Opaque>,
                      layout: Layout,
                      new_size: usize) -> Result<NonNull<Opaque>, AllocErr> {
        let old_size = layout.size();

        if new_size >= old_size {
            if let Ok(()) = self.grow_in_place(ptr, layout.clone(), new_size) {
                return Ok(ptr);
            }
        } else if new_size < old_size {
            if let Ok(()) = self.shrink_in_place(ptr, layout.clone(), new_size) {
                return Ok(ptr);
            }
        }

        // otherwise, fall back on alloc + copy + dealloc.
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let result = self.alloc(new_layout);
        if let Ok(new_ptr) = result {
            ptr::copy_nonoverlapping(ptr.as_ptr() as *const u8,
                                     new_ptr.as_ptr() as *mut u8,
                                     cmp::min(old_size, new_size));
            self.dealloc(ptr, layout);
        }
        result
    }

    /// Behaves like `alloc`, but also ensures that the contents
    /// are set to zero before being returned.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `alloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `alloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn alloc_zeroed(&mut self, layout: Layout) -> Result<NonNull<Opaque>, AllocErr> {
        let size = layout.size();
        let p = self.alloc(layout);
        if let Ok(p) = p {
            ptr::write_bytes(p.as_ptr() as *mut u8, 0, size);
        }
        p
    }

    /// Behaves like `alloc`, but also returns the whole size of
    /// the returned block. For some `layout` inputs, like arrays, this
    /// may include extra storage usable for additional data.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `alloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `alloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn alloc_excess(&mut self, layout: Layout) -> Result<Excess, AllocErr> {
        let usable_size = self.usable_size(&layout);
        self.alloc(layout).map(|p| Excess(p, usable_size.1))
    }

    /// Behaves like `realloc`, but also returns the whole size of
    /// the returned block. For some `layout` inputs, like arrays, this
    /// may include extra storage usable for additional data.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `realloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `realloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn realloc_excess(&mut self,
                             ptr: NonNull<Opaque>,
                             layout: Layout,
                             new_size: usize) -> Result<Excess, AllocErr> {
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let usable_size = self.usable_size(&new_layout);
        self.realloc(ptr, layout, new_size)
            .map(|p| Excess(p, usable_size.1))
    }

    /// Attempts to extend the allocation referenced by `ptr` to fit `new_size`.
    ///
    /// If this returns `Ok`, then the allocator has asserted that the
    /// memory block referenced by `ptr` now fits `new_size`, and thus can
    /// be used to carry data of a layout of that size and same alignment as
    /// `layout`. (The allocator is allowed to
    /// expend effort to accomplish this, such as extending the memory block to
    /// include successor blocks, or virtual memory tricks.)
    ///
    /// Regardless of what this method returns, ownership of the
    /// memory block referenced by `ptr` has not been transferred, and
    /// the contents of the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * `layout` must *fit* the `ptr` (see above); note the
    ///   `new_size` argument need not fit it,
    ///
    /// * `new_size` must not be less than `layout.size()`,
    ///
    /// # Errors
    ///
    /// Returns `Err(CannotReallocInPlace)` when the allocator is
    /// unable to assert that the memory block referenced by `ptr`
    /// could fit `layout`.
    ///
    /// Note that one cannot pass `CannotReallocInPlace` to the `oom`
    /// method; clients are expected either to be able to recover from
    /// `grow_in_place` failures without aborting, or to fall back on
    /// another reallocation method before resorting to an abort.
    unsafe fn grow_in_place(&mut self,
                            ptr: NonNull<Opaque>,
                            layout: Layout,
                            new_size: usize) -> Result<(), CannotReallocInPlace> {
        let _ = ptr; // this default implementation doesn't care about the actual address.
        debug_assert!(new_size >= layout.size);
        let (_l, u) = self.usable_size(&layout);
        // _l <= layout.size()                       [guaranteed by usable_size()]
        //       layout.size() <= new_layout.size()  [required by this method]
        if new_size <= u {
            return Ok(());
        } else {
            return Err(CannotReallocInPlace);
        }
    }

    /// Attempts to shrink the allocation referenced by `ptr` to fit `new_size`.
    ///
    /// If this returns `Ok`, then the allocator has asserted that the
    /// memory block referenced by `ptr` now fits `new_size`, and
    /// thus can only be used to carry data of that smaller
    /// layout. (The allocator is allowed to take advantage of this,
    /// carving off portions of the block for reuse elsewhere.) The
    /// truncated contents of the block within the smaller layout are
    /// unaltered, and ownership of block has not been transferred.
    ///
    /// If this returns `Err`, then the memory block is considered to
    /// still represent the original (larger) `layout`. None of the
    /// block has been carved off for reuse elsewhere, ownership of
    /// the memory block has not been transferred, and the contents of
    /// the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * `layout` must *fit* the `ptr` (see above); note the
    ///   `new_size` argument need not fit it,
    ///
    /// * `new_size` must not be greater than `layout.size()`
    ///   (and must be greater than zero),
    ///
    /// # Errors
    ///
    /// Returns `Err(CannotReallocInPlace)` when the allocator is
    /// unable to assert that the memory block referenced by `ptr`
    /// could fit `layout`.
    ///
    /// Note that one cannot pass `CannotReallocInPlace` to the `oom`
    /// method; clients are expected either to be able to recover from
    /// `shrink_in_place` failures without aborting, or to fall back
    /// on another reallocation method before resorting to an abort.
    unsafe fn shrink_in_place(&mut self,
                              ptr: NonNull<Opaque>,
                              layout: Layout,
                              new_size: usize) -> Result<(), CannotReallocInPlace> {
        let _ = ptr; // this default implementation doesn't care about the actual address.
        debug_assert!(new_size <= layout.size);
        let (l, _u) = self.usable_size(&layout);
        //                      layout.size() <= _u  [guaranteed by usable_size()]
        // new_layout.size() <= layout.size()        [required by this method]
        if l <= new_size {
            return Ok(());
        } else {
            return Err(CannotReallocInPlace);
        }
    }


    // == COMMON USAGE PATTERNS ==
    // alloc_one, dealloc_one, alloc_array, realloc_array. dealloc_array

    /// Allocates a block suitable for holding an instance of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// Note to implementors: If this returns `Ok(ptr)`, then `ptr`
    /// must be considered "currently allocated" and must be
    /// acceptable input to methods such as `realloc` or `dealloc`,
    /// *even if* `T` is a zero-sized type. In other words, if your
    /// `Alloc` implementation overrides this method in a manner
    /// that can return a zero-sized `ptr`, then all reallocation and
    /// deallocation methods need to be similarly overridden to accept
    /// such values as input.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `T` does not meet allocator's size or alignment constraints.
    ///
    /// For zero-sized `T`, may return either of `Ok` or `Err`, but
    /// will *not* yield undefined behavior.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    fn alloc_one<T>(&mut self) -> Result<NonNull<T>, AllocErr>
        where Self: Sized
    {
        let k = Layout::new::<T>();
        if k.size() > 0 {
            unsafe { self.alloc(k).map(|p| p.cast()) }
        } else {
            Err(AllocErr)
        }
    }

    /// Deallocates a block suitable for holding an instance of `T`.
    ///
    /// The given block must have been produced by this allocator,
    /// and must be suitable for storing a `T` (in terms of alignment
    /// as well as minimum and maximum size); otherwise yields
    /// undefined behavior.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure both:
    ///
    /// * `ptr` must denote a block of memory currently allocated via this allocator
    ///
    /// * the layout of `T` must *fit* that block of memory.
    unsafe fn dealloc_one<T>(&mut self, ptr: NonNull<T>)
        where Self: Sized
    {
        let k = Layout::new::<T>();
        if k.size() > 0 {
            self.dealloc(ptr.as_opaque(), k);
        }
    }

    /// Allocates a block suitable for holding `n` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// Note to implementors: If this returns `Ok(ptr)`, then `ptr`
    /// must be considered "currently allocated" and must be
    /// acceptable input to methods such as `realloc` or `dealloc`,
    /// *even if* `T` is a zero-sized type. In other words, if your
    /// `Alloc` implementation overrides this method in a manner
    /// that can return a zero-sized `ptr`, then all reallocation and
    /// deallocation methods need to be similarly overridden to accept
    /// such values as input.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `[T; n]` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// For zero-sized `T` or `n == 0`, may return either of `Ok` or
    /// `Err`, but will *not* yield undefined behavior.
    ///
    /// Always returns `Err` on arithmetic overflow.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    fn alloc_array<T>(&mut self, n: usize) -> Result<NonNull<T>, AllocErr>
        where Self: Sized
    {
        match Layout::array::<T>(n) {
            Ok(ref layout) if layout.size() > 0 => {
                unsafe {
                    self.alloc(layout.clone()).map(|p| p.cast())
                }
            }
            _ => Err(AllocErr),
        }
    }

    /// Reallocates a block previously suitable for holding `n_old`
    /// instances of `T`, returning a block suitable for holding
    /// `n_new` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * the layout of `[T; n_old]` must *fit* that block of memory.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `[T; n_new]` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// For zero-sized `T` or `n_new == 0`, may return either of `Ok` or
    /// `Err`, but will *not* yield undefined behavior.
    ///
    /// Always returns `Err` on arithmetic overflow.
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn realloc_array<T>(&mut self,
                               ptr: NonNull<T>,
                               n_old: usize,
                               n_new: usize) -> Result<NonNull<T>, AllocErr>
        where Self: Sized
    {
        match (Layout::array::<T>(n_old), Layout::array::<T>(n_new)) {
            (Ok(ref k_old), Ok(ref k_new)) if k_old.size() > 0 && k_new.size() > 0 => {
                debug_assert!(k_old.align() == k_new.align());
                self.realloc(ptr.as_opaque(), k_old.clone(), k_new.size()).map(NonNull::cast)
            }
            _ => {
                Err(AllocErr)
            }
        }
    }

    /// Deallocates a block suitable for holding `n` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure both:
    ///
    /// * `ptr` must denote a block of memory currently allocated via this allocator
    ///
    /// * the layout of `[T; n]` must *fit* that block of memory.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either `[T; n]` or the given
    /// memory block does not meet allocator's size or alignment
    /// constraints.
    ///
    /// Always returns `Err` on arithmetic overflow.
    unsafe fn dealloc_array<T>(&mut self, ptr: NonNull<T>, n: usize) -> Result<(), AllocErr>
        where Self: Sized
    {
        match Layout::array::<T>(n) {
            Ok(ref k) if k.size() > 0 => {
                Ok(self.dealloc(ptr.as_opaque(), k.clone()))
            }
            _ => {
                Err(AllocErr)
            }
        }
    }
}
