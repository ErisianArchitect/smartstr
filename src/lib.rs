use core::{
    alloc::Layout,
    mem::{ManuallyDrop, transmute},
    ptr::{self, NonNull},
    slice,
};
use lolevel::checks;
use std::{alloc, mem::offset_of, rc::Rc, slice::SliceIndex, sync::Arc};

// TODO: Maybe make this library work for other pointer widths?
#[cfg(not(target_pointer_width = "64"))]
compile_error!("This library expects a pointer width of 8. Sorry.");

#[must_use]
#[inline(always)]
const fn forgotten<T>(value: T) -> ManuallyDrop<T> {
    ManuallyDrop::new(value)
}

const unsafe fn make_raw_str(ptr: *const u8, len: usize) -> &'static str {
    const _: () = lolevel::checks::assert_same_size_align::<*const str, &[u8]>();
    unsafe {
        let bytes: &[u8] = slice::from_raw_parts(ptr, len);
        std::str::from_utf8_unchecked(bytes)
    }
}

#[must_use]
#[inline(always)]
const unsafe fn make_str_ptr(ptr: *const u8, len: usize) -> *const str {
    const _: () = lolevel::checks::assert_same_size_align::<*const str, &[u8]>();
    unsafe {
        let bytes: &[u8] = slice::from_raw_parts(ptr, len);
        bytes as *const [u8] as *const str
    }
}

#[must_use]
#[inline(always)]
const fn heap_string_layout(size: u32) -> Layout {
    // Use 16 byte alignment because it is known that the heap strings will be at least 16 bytes.
    unsafe { Layout::from_size_align_unchecked(size as usize, 16) }
}

/// The maximum length of a stack [SmartStr] is only 15, so we use this enum
/// for that invariant. This allows the [SmartStr] type to have 240 niches.
/// [StrLen::Addr] is used to represent that this is an [Indirect] type.
/// [Indirect] types store the string in an indirect buffer such as heap
/// or static memory.
#[allow(unused)] // This is just for enforcing niches.
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum StrLen {
    Addr = 0x00,
    Lx01 = 0x01,
    Lx02 = 0x02,
    Lx03 = 0x03,
    Lx04 = 0x04,
    Lx05 = 0x05,
    Lx06 = 0x06,
    Lx07 = 0x07,
    Lx08 = 0x08,
    Lx09 = 0x09,
    Lx0A = 0x0A,
    Lx0B = 0x0B,
    Lx0C = 0x0c,
    Lx0D = 0x0D,
    Lx0E = 0x0E,
    Lx0F = 0x0F,
}

impl StrLen {
    pub const ADDR: usize = Self::Addr as usize;
}

mod footer {
    use super::StrLen;

    #[repr(transparent)]
    #[derive(Debug, Clone, Copy)]
    pub struct Footer(StrLen);
    const _: () = lolevel::checks::assert_same_size_align::<Footer, u8>();

    /// The purpose of [INDIRECT_FOOTER] is to constrain a byte to
    /// the value of [StrLen::Addr].
    pub const INDIRECT_FOOTER: Footer = Footer(StrLen::Addr);
}

use footer::{Footer, INDIRECT_FOOTER};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndirectType {
    Empty = 0,
    Static = 1,
    Heap = 2,
    Box = 3,
    Arc = 4,
    Rc = 5,
}

impl IndirectType {
    #[must_use]
    #[inline(always)]
    const fn into_storage(self) -> StorageType {
        StorageType::Indirect(self)
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_empty(self) -> bool {
        matches!(self, Self::Empty)
    }
}

// NOTE: This type's variants depend on the variants of IndirectType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageType {
    Indirect(IndirectType),
    Inline,
}
const _: () = lolevel::checks::assert_same_size_align::<IndirectType, u8>();
const _: () = lolevel::checks::assert_same_size_align::<StorageType, u8>();

impl StorageType {
    #[must_use]
    #[inline(always)]
    pub const fn is_empty(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Empty))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_static(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Static))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_heap(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Heap))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_box(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Box))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_arc(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Arc))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_rc(self) -> bool {
        matches!(self, Self::Indirect(IndirectType::Rc))
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_inline(self) -> bool {
        matches!(self, Self::Inline)
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct IndirectFlags(u16);

macro_rules! indirect_flags {
    ($(
        <$upper:ident $lower:ident> = $bits:expr
    ),*$(,)?) => {
        paste::paste!{
            $(
                pub const $upper: Self = Self($bits);

                #[must_use]
                #[inline]
                pub const fn $lower(self) -> bool {
                    self.has_all(Self::$upper)
                }

                #[inline]
                pub const fn [<set_ $lower>](&mut self, on: bool) {
                    self.set(Self::$upper, on)
                }

                #[must_use]
                #[inline]
                pub const fn [<with_ $lower>](self) -> Self {
                    self.with(Self::$upper)
                }

                #[must_use]
                #[inline]
                pub const fn [<without_ $lower>](self) -> Self {
                    self.without(Self::$upper)
                }
            )*
        }
    };
}

impl IndirectFlags {
    pub const NONE: Self = Self(0);
    #[must_use]
    #[inline(always)]
    pub const fn invert(self) -> Self {
        Self(!self.0)
    }

    #[inline]
    pub const fn set(&mut self, flags: Self, on: bool) {
        if on {
            self.0 |= flags.0;
        } else {
            self.0 &= flags.invert().0;
        }
    }

    #[must_use]
    #[inline]
    pub const fn with(mut self, flags: Self) -> Self {
        self.set(flags, true);
        self
    }

    #[must_use]
    #[inline(always)]
    pub const fn without(mut self, flags: Self) -> Self {
        self.set(flags, false);
        self
    }

    #[must_use]
    #[inline(always)]
    pub const fn has_any(self, flags: Self) -> bool {
        self.0 & flags.0 != 0
    }

    #[must_use]
    #[inline(always)]
    pub const fn has_all(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }

    #[must_use]
    #[inline(always)]
    pub const fn has_none(self, flags: Self) -> bool {
        self.0 & flags.0 == 0
    }

    indirect_flags! {
        <LEAK leak> = 0x01,
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct Ptr(Option<NonNull<u8>>);
const _: () = lolevel::checks::assert_pointer_size_align::<Ptr>();

impl Ptr {
    pub const NONE: Self = Self(None);

    #[must_use]
    #[inline(always)]
    const fn from_ptr(ptr: *const u8) -> Self {
        unsafe { transmute(ptr) }
    }

    #[must_use]
    #[inline(always)]
    const fn as_ptr(self) -> *const u8 {
        unsafe { transmute(self) }
    }
}

/// When the length of a string is less than or equal to
/// 8, this can be used to easily compare the two strings.
#[repr(C, packed)]
#[derive(Clone, Copy)]
union InlineFast {
    // This is currently unused, but the plan was to make it
    // so that short strings could be compared quickly.
    fast: u64,
    bytes: [u8; 15],
}

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Inline {
    fast: InlineFast,
    len: StrLen,
}
const _: () = lolevel::checks::const_assert(offset_of!(Inline, len) == 15);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Indirect {
    ptr: Ptr,
    _force_align_0: [u64; 0],
    len: u32,
    flags: IndirectFlags,
    ty: IndirectType,
    _footer: Footer,
}
const _: () = checks::assert_pointer_size_align::<Ptr>();
const _: () = checks::const_assert(offset_of!(Indirect, len) == 8);
const _: () = checks::const_assert(offset_of!(Indirect, _footer) == 15);

impl Indirect {
    const MAX_LEN: usize = u32::MAX as usize;
    const EMPTY: Self =
        Indirect::with_footer(Ptr::NONE, 0, IndirectFlags::NONE, IndirectType::Empty);

    #[must_use]
    #[inline(always)]
    const fn with_footer(ptr: Ptr, len: u32, flags: IndirectFlags, ty: IndirectType) -> Self {
        Self {
            ptr,
            _force_align_0: [],
            len,
            flags,
            ty,
            _footer: INDIRECT_FOOTER,
        }
    }

    const unsafe fn new_static(s: &'static str) -> Self {
        Self::with_footer(
            Ptr::from_ptr(s.as_ptr()),
            s.len() as u32,
            IndirectFlags::NONE,
            IndirectType::Static,
        )
    }

    #[must_use]
    #[inline(always)]
    const fn into_smartstr(self) -> SmartStr {
        const _: () = lolevel::checks::assert_same_size_align::<Indirect, SmartStr>();
        unsafe { transmute(self) }
    }

    #[must_use]
    #[inline(always)]
    const fn as_inline(&self) -> &Inline {
        unsafe { transmute(self) }
    }

    unsafe fn forgotten_box(&self) -> ManuallyDrop<Box<str>> {
        debug_assert!(matches!(self.ty, IndirectType::Box));
        forgotten(unsafe {
            Box::<str>::from_raw(make_str_ptr(self.ptr.as_ptr(), self.len()).cast_mut())
        })
    }

    unsafe fn forgotten_arc(&self) -> ManuallyDrop<Arc<str>> {
        debug_assert!(matches!(self.ty, IndirectType::Arc));
        forgotten(unsafe { Arc::<str>::from_raw(make_str_ptr(self.ptr.as_ptr(), self.len())) })
    }

    unsafe fn forgotten_rc(&self) -> ManuallyDrop<Rc<str>> {
        debug_assert!(matches!(self.ty, IndirectType::Rc));
        forgotten(unsafe { Rc::<str>::from_raw(make_str_ptr(self.ptr.as_ptr(), self.len())) })
    }

    #[must_use]
    #[inline(always)]
    fn as_ptr(&self) -> *const u8 {
        match self.ty {
            IndirectType::Empty => self.as_inline().as_ptr(),
            IndirectType::Static | IndirectType::Heap | IndirectType::Box => self.ptr.as_ptr(),
            IndirectType::Arc => unsafe { self.forgotten_arc().as_ptr() },
            IndirectType::Rc => unsafe { self.forgotten_rc().as_ptr() },
        }
    }

    #[must_use]
    #[inline(always)]
    const fn len(&self) -> usize {
        self.len as usize
    }
}

const _: () = lolevel::checks::assert_same_size_align::<[u64; 2], Inline>();
const _: () = lolevel::checks::assert_same_size_align::<[u64; 2], Indirect>();

impl Inline {
    #[must_use]
    #[inline(always)]
    const fn as_indirect(&self) -> &Indirect {
        unsafe { transmute(self) }
    }

    #[must_use]
    #[inline(always)]
    const fn as_indirect_mut(&mut self) -> &mut Indirect {
        unsafe { transmute(self) }
    }

    #[must_use]
    #[inline]
    const unsafe fn new(bytes: [u8; 15], len: StrLen) -> Self {
        Self {
            fast: InlineFast { bytes },
            len,
        }
    }

    // #[must_use]
    // #[inline]
    // const fn get_fast(&self) -> u64 {
    //     unsafe { self.fast.fast }
    // }

    #[must_use]
    #[inline(always)]
    const fn len(&self) -> usize {
        self.len as usize
    }

    #[must_use]
    #[inline(always)]
    const fn as_ptr(&self) -> *const u8 {
        unsafe { self.fast.bytes.as_ptr() }
    }

    // #[must_use]
    // #[inline(always)]
    // const fn as_str_ptr(&self) -> *const str {
    //     unsafe { make_str_ptr(self.as_ptr(), self.len()) }
    // }

    #[must_use]
    #[inline(always)]
    const fn as_str(&self) -> &str {
        unsafe { make_raw_str(self.as_ptr(), self.len()) }
    }

    #[must_use]
    #[inline(always)]
    const fn into_smartstr(self) -> SmartStr {
        const _SAFETY: () = lolevel::checks::assert_same_size_align::<Inline, SmartStr>();
        unsafe { transmute(self) }
    }
}

#[repr(transparent)]
pub struct SmartStr {
    inline: Inline,
}
const _: () = checks::const_assert(SmartStr::INLINE_LEN == 15);

impl SmartStr {
    pub const INLINE_LEN: usize = size_of::<InlineFast>();

    #[must_use]
    #[inline(always)]
    pub const fn new_empty() -> Self {
        Indirect::EMPTY.into_smartstr()
    }

    const unsafe fn new_inline(s: &str) -> Self {
        debug_assert!(matches!(s.len(), 1..16));
        let mut bytes = [0u8; 15];
        unsafe {
            ptr::copy_nonoverlapping(s.as_ptr(), bytes.as_mut_ptr(), s.len());
            Inline::new(bytes, transmute::<u8, StrLen>(s.len() as u8)).into_smartstr()
        }
    }

    #[must_use]
    pub const fn new_static(s: &'static str) -> Self {
        match s.len() {
            // static is never special variant such as inline or empty.
            0..=Indirect::MAX_LEN => unsafe { Indirect::new_static(s).into_smartstr() },
            _ => panic!("This string is way too fuckin long, buddy"),
        }
    }

    unsafe fn new_heap(s: &str) -> Self {
        debug_assert!(s.len() <= Indirect::MAX_LEN);
        let len = s.len() as u32;
        let layout = heap_string_layout(len);
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            alloc::handle_alloc_error(layout);
        }
        unsafe {
            ptr::copy_nonoverlapping(s.as_ptr(), ptr, s.len());
        }
        Indirect::with_footer(
            Ptr::from_ptr(ptr),
            len,
            IndirectFlags::NONE,
            IndirectType::Heap,
        )
        .into_smartstr()
    }

    #[must_use]
    pub fn new(s: &str) -> Self {
        match s.len() {
            0 => Self::new_empty(),
            1..16 => unsafe { Self::new_inline(s) },
            16..=Indirect::MAX_LEN => unsafe { Self::new_heap(s) },
            _ => panic!("String can't be larger than 2^32-1"),
        }
    }

    pub fn from_box(s: Box<str>) -> Result<Self, Box<str>> {
        if s.len() > Indirect::MAX_LEN {
            return Err(s);
        }
        let leak = Box::leak(s);
        let (ptr, len) = (leak.as_ptr(), leak.len());
        Ok(Indirect::with_footer(
            Ptr::from_ptr(ptr),
            len as u32,
            IndirectFlags::NONE,
            IndirectType::Box,
        )
        .into_smartstr())
    }

    pub fn from_arc(s: Arc<str>) -> Result<Self, Arc<str>> {
        if s.len() > Indirect::MAX_LEN {
            return Err(s);
        }
        let leak = Arc::into_raw(s);
        let s: &'static str = unsafe { transmute(&*leak) };
        let (ptr, len) = (s.as_ptr(), s.len());
        Ok(Indirect::with_footer(
            Ptr::from_ptr(ptr),
            len as u32,
            IndirectFlags::NONE,
            IndirectType::Arc,
        )
        .into_smartstr())
    }

    pub fn from_rc(s: Rc<str>) -> Result<Self, Rc<str>> {
        if s.len() > Indirect::MAX_LEN {
            return Err(s);
        }
        let leak = Rc::into_raw(s);
        let s: &'static str = unsafe { transmute(&*leak) };
        let (ptr, len) = (s.as_ptr(), s.len());
        Ok(Indirect::with_footer(
            Ptr::from_ptr(ptr),
            len as u32,
            IndirectFlags::NONE,
            IndirectType::Rc,
        )
        .into_smartstr())
    }

    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        match self.inline.len() {
            StrLen::ADDR => unsafe { self.indirect().len() },
            len => len,
        }
    }

    pub fn leak(mut self) -> Self {
        if !self.storage_type().is_inline() {
            unsafe { self.indirect_mut() }.flags.set_leak(true);
        }
        self
    }

    pub fn unleak(mut self) -> Self {
        if !self.storage_type().is_inline() {
            unsafe { self.indirect_mut() }.flags.set_leak(false);
        }
        self
    }

    #[must_use]
    pub const fn as_static_str(&self) -> Option<&'static str> {
        if self.is_inline() {
            None
        } else {
            let indirect = self.inline.as_indirect();
            let (ptr, len) = match indirect.ty {
                IndirectType::Empty => return Some(""),
                IndirectType::Static => (indirect.ptr.as_ptr(), indirect.len()),
                _ => return None,
            };
            unsafe { transmute::<&[u8], Option<&'static str>>(slice::from_raw_parts(ptr, len)) }
        }
    }

    pub fn as_ptr(&self) -> *const u8 {
        match self.inline.len {
            StrLen::Addr => unsafe { self.indirect() }.as_ptr(),
            _ => self.inline.as_ptr(),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        if matches!(self.inline.len, StrLen::Addr) {
            let indirect = self.inline.as_indirect();
            let (ptr, len) = match indirect.ty {
                IndirectType::Empty => (self.inline.as_ptr(), 0),
                IndirectType::Static | IndirectType::Heap | IndirectType::Box => {
                    (indirect.ptr.as_ptr(), indirect.len())
                }
                IndirectType::Arc => unsafe {
                    let arc = ManuallyDrop::new(Arc::from_raw(make_str_ptr(
                        indirect.ptr.as_ptr(),
                        indirect.len(),
                    )));
                    (arc.as_ptr(), indirect.len())
                },
                IndirectType::Rc => unsafe {
                    let rc = ManuallyDrop::new(Rc::from_raw(make_str_ptr(
                        indirect.ptr.as_ptr(),
                        indirect.len(),
                    )));
                    (rc.as_ptr(), indirect.len())
                },
            };
            unsafe { std::str::from_utf8_unchecked(slice::from_raw_parts(ptr, len)) }
        } else {
            self.inline.as_str()
        }
    }

    #[must_use]
    #[inline]
    pub const fn storage_type(&self) -> StorageType {
        match self.inline.len {
            StrLen::Addr => self.inline.as_indirect().ty.into_storage(),
            _ => StorageType::Inline,
        }
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.storage_type().is_empty()
    }

    #[must_use]
    #[inline]
    pub const fn is_inline(&self) -> bool {
        self.inline.len as usize != StrLen::ADDR
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_static(&self) -> bool {
        self.storage_type().is_static()
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_heap(&self) -> bool {
        self.storage_type().is_heap()
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_box(&self) -> bool {
        self.storage_type().is_box()
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_arc(&self) -> bool {
        self.storage_type().is_arc()
    }

    #[must_use]
    #[inline(always)]
    pub const fn is_rc(&self) -> bool {
        self.storage_type().is_rc()
    }

    #[must_use]
    #[inline(always)]
    const unsafe fn indirect(&self) -> &Indirect {
        self.inline.as_indirect()
    }

    #[must_use]
    #[inline(always)]
    const unsafe fn indirect_mut(&mut self) -> &mut Indirect {
        self.inline.as_indirect_mut()
    }
}

impl Drop for SmartStr {
    fn drop(&mut self) {
        match self.inline.len as usize {
            StrLen::ADDR => {
                let indirect = unsafe { self.indirect() };
                if indirect.flags.leak() {
                    return;
                }
                match indirect.ty {
                    IndirectType::Empty | IndirectType::Static => (/* happily do nothing */),
                    IndirectType::Heap => {
                        let layout = heap_string_layout(indirect.len);
                        unsafe {
                            std::alloc::dealloc(indirect.ptr.as_ptr().cast_mut(), layout);
                        }
                    }
                    IndirectType::Box => unsafe {
                        let mut boxed = indirect.forgotten_box();
                        ManuallyDrop::drop(&mut boxed);
                    },
                    IndirectType::Arc => unsafe {
                        let mut arc = indirect.forgotten_arc();
                        ManuallyDrop::drop(&mut arc);
                    },
                    IndirectType::Rc => unsafe {
                        let mut rc = indirect.forgotten_rc();
                        ManuallyDrop::drop(&mut rc);
                    },
                }
            }
            _ => (/* inline doesn't get dropped. */),
        }
    }
}

impl std::ops::Deref for SmartStr {
    type Target = str;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for SmartStr {
    #[inline(always)]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for SmartStr {
    #[inline(always)]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<I: SliceIndex<str>> std::ops::Index<I> for SmartStr
where
    str: std::ops::Index<I>,
{
    type Output = <str as std::ops::Index<I>>::Output;

    #[inline(always)]
    fn index(&self, index: I) -> &Self::Output {
        &self.as_str()[index]
    }
}

impl std::fmt::Display for SmartStr {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.as_str(), f)
    }
}

impl std::fmt::Debug for SmartStr {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<S: AsRef<str>> std::cmp::PartialEq<S> for SmartStr {
    #[inline(always)]
    fn eq(&self, other: &S) -> bool {
        self.as_str().eq(other.as_ref())
    }
}

impl std::cmp::PartialEq<str> for SmartStr {
    #[inline(always)]
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

impl std::cmp::Eq for SmartStr {}

impl<S: AsRef<str>> std::cmp::PartialOrd<S> for SmartStr {
    #[inline(always)]
    fn ge(&self, other: &S) -> bool {
        self.as_str().ge(other.as_ref())
    }

    #[inline(always)]
    fn gt(&self, other: &S) -> bool {
        self.as_str().gt(other.as_ref())
    }

    #[inline(always)]
    fn le(&self, other: &S) -> bool {
        self.as_str().le(other.as_ref())
    }

    #[inline(always)]
    fn lt(&self, other: &S) -> bool {
        self.as_str().lt(other.as_ref())
    }

    #[inline(always)]
    fn partial_cmp(&self, other: &S) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other.as_ref())
    }
}

impl std::cmp::PartialOrd<str> for SmartStr {
    #[inline(always)]
    fn ge(&self, other: &str) -> bool {
        self.as_str().ge(other)
    }

    #[inline(always)]
    fn gt(&self, other: &str) -> bool {
        self.as_str().gt(other)
    }

    #[inline(always)]
    fn le(&self, other: &str) -> bool {
        self.as_str().le(other)
    }

    #[inline(always)]
    fn lt(&self, other: &str) -> bool {
        self.as_str().lt(other)
    }

    #[inline(always)]
    fn partial_cmp(&self, other: &str) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl std::cmp::Ord for SmartStr {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl std::hash::Hash for SmartStr {
    #[inline(always)]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<&str> for SmartStr {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<Box<str>> for SmartStr {
    fn from(value: Box<str>) -> Self {
        Self::from_box(value).expect("String was too long.")
    }
}

impl From<Arc<str>> for SmartStr {
    fn from(value: Arc<str>) -> Self {
        Self::from_arc(value).expect("String was too long.")
    }
}

impl From<Rc<str>> for SmartStr {
    fn from(value: Rc<str>) -> Self {
        Self::from_rc(value).expect("String was too long.")
    }
}

impl Clone for SmartStr {
    fn clone(&self) -> Self {
        if matches!(self.inline.len, StrLen::Addr) {
            let indirect = unsafe { self.indirect() };
            match indirect.ty {
                IndirectType::Empty => Self::new_empty(),
                IndirectType::Static => Self {
                    inline: self.inline,
                },
                IndirectType::Heap | IndirectType::Box => unsafe { Self::new_heap(self.as_str()) },
                IndirectType::Arc => unsafe {
                    Self::from_arc(Arc::clone(&indirect.forgotten_arc())).unwrap()
                },
                IndirectType::Rc => unsafe {
                    Self::from_rc(Rc::clone(&indirect.forgotten_rc())).unwrap()
                },
            }
        } else {
            Self {
                inline: self.inline,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn smartstr_test() {
        let hello = SmartStr::new_static("123456789ABCDEF");
        assert!(hello.storage_type().is_static());
        println!("{hello}");
        let bigger = SmartStr::new_static("This is a much longer string, but it is static.");
        assert!(bigger.storage_type().is_static());
        println!("{bigger}");
        let inline = SmartStr::new("inline");
        assert!(inline.storage_type().is_inline());
        assert_eq!(inline, "inline");
        println!("{inline}");
        let longer = SmartStr::new(
            "This is a longer string. I don't know how long it has to be (at least 16 bytes).",
        );
        assert!(longer.storage_type().is_heap());
        println!("{longer}");
        const _: () = lolevel::checks::assert_niche::<SmartStr>();
        const _: () = lolevel::checks::assert_niche::<Option<SmartStr>>();
        const _: () = lolevel::checks::assert_niche::<Option<Option<SmartStr>>>();
        let boxed = SmartStr::from(Box::from("hello, world!"));
        println!("{boxed}");
        assert!(boxed.storage_type().is_box());
        let arc = SmartStr::from(Arc::from("hello, world!"));
        println!("{arc}");
        assert!(arc.storage_type().is_arc());
        let rc = SmartStr::from(Rc::from("\"hello, world!\""));
        println!("{rc}");
        assert!(rc.storage_type().is_rc());
        let foo = SmartStr::new_static("foo\nbar");
        println!("Debug: {foo:?}");

        let mut map = HashMap::<SmartStr, SmartStr>::new();
        map.insert(
            SmartStr::new_static("test"),
            SmartStr::new_static("hello, world!"),
        );
        if let Some(hello) = map.get("test") {
            println!("{hello}");
        }

        assert!(inline.storage_type().is_inline());

        let empty = SmartStr::new("");
        assert!(empty.is_empty());
        assert_eq!(empty, "");
        assert!(ptr::eq(empty.inline.as_ptr(), empty.as_str().as_ptr()));
        assert!(ptr::eq(
            (&empty as *const SmartStr).cast(),
            empty.as_str().as_ptr()
        ));

        assert!(ptr::eq(inline.inline.as_ptr(), inline.as_str().as_ptr()));
        assert!(ptr::eq(inline.as_ptr(), inline.as_str().as_ptr()));
        assert!(ptr::eq(longer.as_ptr(), longer.as_str().as_ptr()));
        assert!(ptr::eq(
            (&inline as *const SmartStr).cast(),
            inline.as_str().as_ptr()
        ));

        let arc2 = arc.clone();
        let arc3 = arc.clone();
        assert!(ptr::eq(arc.as_ptr(), arc2.as_ptr()));
        assert!(ptr::eq(arc.as_ptr(), arc3.as_ptr()));
    }
}
