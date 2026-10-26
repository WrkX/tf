//! sha-256 über commoncrypto (libsystem), damit keine crypto im programm landet.

use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Context {
    count: [u32; 2],
    hash: [u32; 8],
    wbuf: [u32; 16],
}

unsafe extern "C" {
    fn CC_SHA256_Init(context: *mut Context) -> i32;
    fn CC_SHA256_Update(context: *mut Context, data: *const c_void, len: u32) -> i32;
    fn CC_SHA256_Final(digest: *mut u8, context: *mut Context) -> i32;
}

pub struct Sha256 {
    context: Context,
}

impl Sha256 {
    pub fn new() -> Self {
        let mut context = Context { count: [0; 2], hash: [0; 8], wbuf: [0; 16] };
        // SAFETY: `context` ist ein korrekt dimensionierter commoncrypto-context.
        unsafe { CC_SHA256_Init(&mut context) };
        Self { context }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let len = data.len().min(u32::MAX as usize);
            // SAFETY: der slice ist für `len` bytes gültig und der context initialisiert.
            unsafe { CC_SHA256_Update(&mut self.context, data.as_ptr().cast(), len as u32) };
            data = &data[len..];
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let mut digest = [0u8; 32];
        // SAFETY: der digest-buffer hat 32 bytes (`CC_SHA256_DIGEST_LENGTH`).
        unsafe { CC_SHA256_Final(digest.as_mut_ptr(), &mut self.context) };
        digest
    }

    pub fn hex(digest: &[u8; 32]) -> String {
        let mut out = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub fn hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    Sha256::hex(&hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn updates_across_chunks_match_one_shot() {
        let data = vec![0x5au8; 1_000_001];
        let mut hasher = Sha256::new();
        for chunk in data.chunks(7919) {
            hasher.update(chunk);
        }
        assert_eq!(Sha256::hex(&hasher.finish()), hex(&data));
    }
}
