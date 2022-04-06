use std::{convert::TryFrom, io::{self, Read}};

pub const K_CHEAP_PREPEND_SIZE: usize = 8;
pub const K_INITIAL_SIZE: usize = 1024;
const K_READ_BUF_SIZE: usize = 1024;

pub struct Buffer {
    data: Vec<u8>,
    read_idx: usize,
    write_idx: usize,
}

pub struct Builder {
    initial_size: usize,
}

impl Builder {
    pub fn new() -> Self {
        Builder {
            initial_size: K_INITIAL_SIZE,
        }
    }

    pub fn size(mut self, size: usize) -> Self {
        self.initial_size = size;
        self
    }

    pub fn build(self) -> Buffer {
        Buffer {
            data: vec![0; K_CHEAP_PREPEND_SIZE + self.initial_size], // len:n, cap:n
            read_idx: K_CHEAP_PREPEND_SIZE,
            write_idx: K_CHEAP_PREPEND_SIZE,
        }
    }
}


// TODO: type Array: TryFrom; ???
// macro_rules! impl_peek_as_for_int {
//     ($($int:ty, $to_int:ident),*) => {
//         $(pub fn $to_int(&self) -> $int {
//             const N: usize = std::mem::size_of::<$int>();
//             assert!(N <= self.readable_bytes());
//             let mut dst = [0u8; N];
//             dst.clone_from_slice(&self.peek()[..N]);
//             <$int>::from_be_bytes(dst)
//         })*
//     };
// }

// macro_rules! impl_retrieve_as_for_int {
//     ($($int:ty, $to_int:ident),*) => {
//         $(pub fn $to_int(&mut self) -> $int {
//             const N: usize = std::mem::size_of::<$int>();
//             let mut dst = [0u8; N];
//             dst.clone_from_slice(&self.peek()[..N]);
//             let ret = <$int>::from_be_bytes(dst);
//             self.retrieve(N);
//             ret
//         })*
//     };
// }

impl Buffer {
    pub fn new() -> Self {
        Builder::new().build()
    }

    pub fn read_from<T: Read>(&mut self, io: &mut T) -> io::Result<usize> {
        let mut buf = [0u8; K_READ_BUF_SIZE];
        let n = io.read(&mut buf)?;
        self.append(&buf[..n]); // todo: optimize copy case
        Ok(n)
    }

    pub fn readable_bytes(&self) -> usize {
        self.write_idx - self.read_idx
    }

    pub fn writable_bytes(&self) -> usize {
        self.data.len() - self.write_idx
    }

    pub fn prependable_bytes(&self) -> usize {
        self.read_idx
    }

    pub fn retrieve(&mut self, size: usize) {
        assert!(size <= self.readable_bytes());
        if size < self.readable_bytes() {
            self.read_idx += size;
        } else {
            self.retrieve_all();
        }
    }

    pub fn retrieve_all(&mut self) {
        self.read_idx = K_CHEAP_PREPEND_SIZE;
        self.write_idx = K_CHEAP_PREPEND_SIZE;
    }

    // prepend with copy of data
    // TODO: overload?
    pub fn prepend(&mut self, data: &[u8]) {
        let len = data.len();
        assert!(len <= self.prependable_bytes());
        self.read_idx -= len;
        self.data.splice(self.read_idx..self.read_idx+len, data.iter().cloned());
    }

    // append with copy of data
    // TODO: overload?
    pub fn append(&mut self, data: &[u8]) {
        let len = data.len();
        self.ensure_writable(len);
        self.data.splice(self.write_idx..self.write_idx+len, data.iter().cloned());
        self.write_idx += len;
    }

    fn ensure_writable(&mut self, len: usize) {
        // more space
        if self.writable_bytes() < len {
            // in case readIdx becomes larger after lots of ops, prependable becomes large, too
            // we don't want to alloc more space but move content-already ahead, and save space
            // for writable
            // case #1. prependable + writable cannot afford, we still need to alloc
            if self.writable_bytes() + self.prependable_bytes() < len + K_CHEAP_PREPEND_SIZE {
                // TODO: resize, but default bzero-ed is no-need, well it's slight perf improve
                self.data.resize(self.write_idx + len, 0);
            }
            // case #2. move content-already ahead to idx of KCHEAP_PREPEND_SIZE
            else {
                assert!(K_CHEAP_PREPEND_SIZE < self.read_idx);
                let readable = self.readable_bytes();
                self.data
                    .copy_within(self.read_idx..self.write_idx, K_CHEAP_PREPEND_SIZE);
                self.read_idx = K_CHEAP_PREPEND_SIZE;
                self.write_idx = self.read_idx + readable;
                assert_eq!(readable, self.readable_bytes());
            }
        }
        assert!(len <= self.writable_bytes());
    }

    pub fn peek(&self) -> &[u8] {
        &self.data.as_slice()[self.read_idx..self.write_idx]
    }

    // impl_peek_as_for_int!(
    //     i8, peek_as_i8,
    //     i16, peek_as_i16,
    //     i32, peek_as_i32,
    //     i64, peek_as_i64);
    // impl_retrieve_as_for_int!(
    //     i8, retrieve_as_i8,
    //     i16, retrieve_as_i16,
    //     i32, retrieve_as_i32,
    //     i64, retrieve_as_i64);
    pub fn peek_as<T: EndianRead>(&self) -> T {
        let len = std::mem::size_of::<T>();
        assert!(len <= self.readable_bytes());
        let s = &self.peek()[..len];
        match T::Array::try_from(s) {
            Ok(b) => T::from_bytes(b),
            Err(_) => panic!("impossible! slice -> array"),
        }
    }

    pub fn peek_as_string(&self, len: usize) -> String {
        assert!(len <= self.readable_bytes());
        let bytes = &self.peek()[..len];
        String::from_utf8_lossy(bytes).into_owned()
    }

    pub fn retrieve_as<T: EndianRead>(&mut self) -> T {
        let len = std::mem::size_of::<T>();
        let ret = self.peek_as::<T>();
        self.retrieve(len);
        ret
    }

    pub fn retrieve_as_string(&mut self, len: usize) -> String {
        let ret = self.peek_as_string(len);
        self.retrieve(len);
        ret
    }

    pub fn retrieve_all_as_string(&mut self) -> String {
        let bytes = self.peek();
        let ret = String::from_utf8_lossy(bytes).into_owned();
        self.retrieve_all();
        ret
    }

    pub fn append_as<T: EndianWrite>(&mut self, val: T) {
        let bytes = val.as_bytes();
        self.append(bytes.as_ref())
    }

    pub fn append_string(&mut self, val: String) {
        self.append(val.as_bytes())
    }

    pub fn prepend_as<T: EndianWrite>(&mut self, val: T) {
        let bytes = val.as_bytes();
        self.prepend(bytes.as_ref())
    }
}

pub trait EndianWrite {
    type Array: AsRef<[u8]>;
    fn as_bytes(self) -> Self::Array;
}
macro_rules! impl_endian_write_for_int {
    ($($int:ty),*) => {$(
        impl EndianWrite for $int {
            type Array = [u8; std::mem::size_of::<$int>()];
            fn as_bytes(self) -> Self::Array {
                self.to_be_bytes()
            }
        })*
    };
}
impl_endian_write_for_int!(i8, i16, i32, i64);

pub trait EndianRead {
    type Array: for <'a> TryFrom<&'a [u8]>;
    fn from_bytes(b: Self::Array) -> Self;
}
macro_rules! impl_endian_read_for_int {
    ($($int:ty),*) => {$(
        impl EndianRead for $int {
            type Array = [u8; std::mem::size_of::<$int>()];
            fn from_bytes(b: Self::Array) -> Self {
                Self::from_be_bytes(b)
            }
        })*
    };
}
impl_endian_read_for_int!(i8, i16, i32, i64);


#[cfg(test)]
mod tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn test_buffer_append_retrieve() {
        let mut buf = Buffer::new();
        assert_eq!(buf.readable_bytes(), 0);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE);

        // write 200
        let i = ['x'; 200].iter();
        let str = String::from_iter(i);
        buf.append(str.as_bytes());
        assert_eq!(buf.readable_bytes(), str.len());
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - str.len());

        // retrieve 50
        let str2 = buf.retrieve_as_string(50);
        assert_eq!(str2.len(), 50);
        assert_eq!(buf.readable_bytes(), str.len() - str2.len());
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE + str2.len());
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - str.len());
        assert_eq!(str2, String::from_iter(['x'; 50].iter()));

        // write 200 again
        buf.append(str.as_bytes());
        assert_eq!(buf.readable_bytes(), 2*str.len() - str2.len());
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE + str2.len());
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - 2*str.len());

        // retrieve all
        let str3 = buf.retrieve_all_as_string();
        assert_eq!(str3.len(), 350);
        assert_eq!(buf.readable_bytes(), 0);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE);
        assert_eq!(str3, String::from_iter(['x'; 350].iter()));
    }

    #[test]
    fn test_buffer_grow() {
        let mut buf = Buffer::new();
        let s = ['y' as u8; 400];
        buf.append(&s);

        assert_eq!(buf.readable_bytes(), 400);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - 400);

        // retrieve 50
        buf.retrieve(50);
        assert_eq!(buf.readable_bytes(), 350);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE + 50);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - 400);

        // append 1000 (cannot afford) 58 + 1350 + 0
        let s2 = ['z' as u8; 1000];
        buf.append(&s2);
        assert_eq!(buf.readable_bytes(), 1350);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE + 50); // 58
        assert_eq!(buf.writable_bytes(), 0);

        // append 30 (can afford) 8 + 1380 + 20
        let s3 = ['0' as u8; 30];
        buf.append(&s3);
        assert_eq!(buf.readable_bytes(), 1380);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE); // 8
        assert_eq!(buf.writable_bytes(), 20); // 50 - 30

        buf.retrieve_all();
        assert_eq!(buf.readable_bytes(), 0);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE);
        assert_eq!(buf.writable_bytes(), 1400);
    }

    #[test]
    fn test_buffer_prepend() {
        let mut buf = Buffer::new();
        let s = ['y' as u8; 200];
        buf.append(&s);
        assert_eq!(buf.readable_bytes(), 200);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - 200);

        let x: i32 = 4;
        buf.prepend_as(x);
        assert_eq!(buf.readable_bytes(), 204);
        assert_eq!(buf.prependable_bytes(), K_CHEAP_PREPEND_SIZE - 4);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE - 200);
    }

    #[test]
    fn test_buffer_read_write_int() {
        let mut buf = Buffer::new();
        let s = String::from("HTTP");
        buf.append_string(s);
        assert_eq!(buf.readable_bytes(), 4);

        assert_eq!(buf.peek_as::<i8>(), 'H' as i8);
        let top16: i16 = buf.peek_as();
        assert_eq!(top16, ('H' as i16 * 256) + 'T' as i16);
        assert_eq!(buf.peek_as::<i32>(), top16 as i32 * 65536 + ('T' as i32 * 256) + ('P' as i32));

        assert_eq!(buf.retrieve_as::<i8>(), 'H' as i8);
        assert_eq!(buf.retrieve_as::<i16>(), 'T' as i16 * 256 + 'T' as i16);
        assert_eq!(buf.retrieve_as::<i8>(), 'P' as i8);
        assert_eq!(buf.readable_bytes(), 0);
        assert_eq!(buf.writable_bytes(), K_INITIAL_SIZE);

        buf.append_as(-1 as i8);
        buf.append_as(-2 as i16);
        buf.append_as(-3 as i32);
        assert_eq!(buf.readable_bytes(), 7);
        assert_eq!(buf.retrieve_as::<i8>(), -1 as i8);
        assert_eq!(buf.retrieve_as::<i16>(), -2 as i16);
        assert_eq!(buf.retrieve_as::<i32>(), -3 as i32);
    }
}
