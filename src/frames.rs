// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

use std::borrow::Cow;
use std::fmt::{self, Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::os::raw::c_void;
use std::path::PathBuf;

use backtrace::Frame;
use symbolic_demangle::demangle;

use crate::{MAX_DEPTH, MAX_THREAD_NAME};

#[derive(Debug, Clone)]
pub struct UnresolvedFramesSlice<'a> {
    pub frames: &'a [Frame],
    pub thread_name: &'a [u8],
    pub thread_id: u64,
}

pub struct UnresolvedFrames {
    pub frames: [Frame; MAX_DEPTH],
    pub depth: usize,
    pub thread_name: [u8; MAX_THREAD_NAME],
    pub thread_name_length: usize,
    pub thread_id: u64,
}

impl Default for UnresolvedFrames {
    #[allow(clippy::uninit_assumed_init)]
    fn default() -> Self {
        let frames = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        Self {
            frames,
            depth: 0,
            thread_name: [0; MAX_THREAD_NAME],
            thread_name_length: 0,
            thread_id: 0,
        }
    }
}

impl Clone for UnresolvedFrames {
    fn clone(&self) -> Self {
        let slice = self.slice().clone();
        Self::new(slice.frames, slice.thread_name, slice.thread_id)
    }
}

impl Debug for UnresolvedFrames {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        self.slice().fmt(f)
    }
}

impl UnresolvedFrames {
    #[allow(clippy::uninit_assumed_init)]
    pub fn new(bt: &[Frame], tn: &[u8], thread_id: u64) -> Self {
        let depth = bt.len();
        let mut frames: [Frame; MAX_DEPTH] =
            unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        frames[0..depth].clone_from_slice(bt);

        let thread_name_length = tn.len();
        let mut thread_name = [0; MAX_THREAD_NAME];
        thread_name[0..thread_name_length].clone_from_slice(tn);

        Self {
            frames,
            depth,
            thread_name,
            thread_name_length,
            thread_id,
        }
    }

    fn slice(&self) -> UnresolvedFramesSlice {
        UnresolvedFramesSlice {
            frames: &self.frames[0..self.depth],
            thread_name: &self.thread_name[0..self.thread_name_length],
            thread_id: self.thread_id,
        }
    }
}

impl PartialEq for UnresolvedFrames {
    fn eq(&self, other: &Self) -> bool {
        let (frames1, frames2) = (self.slice().frames, other.slice().frames);
        if self.thread_id != other.thread_id || frames1.len() != frames2.len() {
            false
        } else {
            Iterator::zip(frames1.iter(), frames2.iter())
                .map(|(s1, s2)| s1.symbol_address() == s2.symbol_address())
                .all(|equal| equal)
        }
    }
}

impl Eq for UnresolvedFrames {}

impl Hash for UnresolvedFrames {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.slice()
            .frames
            .iter()
            .for_each(|frame| frame.symbol_address().hash(state));
        self.thread_id.hash(state);
    }
}

/// Symbol is a representation of a function symbol. It contains name and addr of it. If built with
/// debug message, it can also provide line number and filename. The name in it is not demangled.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// This name is raw name of a symbol (which hasn't been demangled).
    pub name: Option<Vec<u8>>,

    /// The address of the function. It is not 100% trustworthy.
    pub addr: Option<*mut c_void>,

    /// Line number of this symbol. If compiled with debug message, you can get it.
    pub lineno: Option<u32>,

    /// Filename of this symbol. If compiled with debug message, you can get it.
    pub filename: Option<PathBuf>,
}

impl Symbol {
    pub fn raw_name(&self) -> &[u8] {
        self.name.as_deref().unwrap_or(b"Unknow")
    }

    pub fn name(&self) -> String {
        demangle(&String::from_utf8_lossy(self.raw_name())).into_owned()
    }

    pub fn sys_name(&self) -> Cow<str> {
        String::from_utf8_lossy(self.raw_name())
    }

    pub fn filename(&self) -> Cow<str> {
        self.filename
            .as_ref()
            .map(|name| name.as_os_str().to_string_lossy())
            .unwrap_or_else(|| Cow::Borrowed("Unknow"))
    }

    pub fn lineno(&self) -> u32 {
        self.lineno.unwrap_or(0)
    }
}

unsafe impl Send for Symbol {}

impl From<&backtrace::Symbol> for Symbol {
    fn from(symbol: &backtrace::Symbol) -> Self {
        Symbol {
            name: symbol.name().map(|name| name.as_bytes().to_vec()),
            addr: symbol.addr(),
            lineno: symbol.lineno(),
            filename: symbol.filename().map(|filename| filename.to_owned()),
        }
    }
}

impl Display for Symbol {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(&self.name())
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool {
        self.raw_name() == other.raw_name()
    }
}

impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw_name().hash(state)
    }
}

/// A representation of a backtrace. `thread_name` and `thread_id` was got from `pthread_getname_np`
/// and `pthread_self`. frames is a vector of symbols.
#[derive(Clone, PartialEq, Hash)]
pub struct Frames {
    pub frames: Vec<Vec<Symbol>>,
    pub thread_name: String,
    pub thread_id: u64,
}

impl From<UnresolvedFrames> for Frames {
    fn from(frames: UnresolvedFrames) -> Self {
        let mut fs = Vec::new();

        let mut frame_iter = frames.slice().frames.iter();

        while let Some(frame) = frame_iter.next() {
            let mut symbols = Vec::new();

            backtrace::resolve_frame(frame, |symbol| {
                let symbol = Symbol::from(symbol);
                symbols.push(symbol);
            });

            if symbols
                .iter()
                .any(|symbol| symbol.name() == "perf_signal_handler")
            {
                // ignore frame itself and its next one
                frame_iter.next();
                continue;
            }

            if !symbols.is_empty() {
                fs.push(symbols);
            }
        }

        Self {
            frames: fs,
            thread_name: String::from_utf8_lossy(&frames.thread_name[0..frames.thread_name_length])
                .into_owned(),
            thread_id: frames.thread_id,
        }
    }
}

impl Eq for Frames {}

impl Debug for Frames {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        for frame in self.frames.iter() {
            write!(f, "FRAME: ")?;
            for symbol in frame.iter() {
                write!(f, "{} -> ", symbol)?;
            }
        }
        write!(f, "THREAD: ")?;
        if !self.thread_name.is_empty() {
            write!(f, "{}", self.thread_name)
        } else {
            write!(f, "ThreadId({})", self.thread_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demangle_rust() {
        let symbol = Symbol {
            name: Some(b"_ZN3foo3barE".to_vec()),
            addr: None,
            lineno: None,
            filename: None,
        };

        assert_eq!(&symbol.name(), "foo::bar")
    }

    #[test]
    fn demangle_cpp() {
        let name =
            b"_ZNK3MapI10StringName3RefI8GDScriptE10ComparatorIS0_E16DefaultAllocatorE3hasERKS0_"
                .to_vec();

        let symbol = Symbol {
            name: Some(name),
            addr: None,
            lineno: None,
            filename: None,
        };

        assert_eq!(
            &symbol.name(),
            "Map<StringName, Ref<GDScript>, Comparator<StringName>, DefaultAllocator>::has(StringName const&) const"
        )
    }
}
