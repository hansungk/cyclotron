#![allow(unused)]
#![allow(unused_macros)]

use log::info;
use num_traits::{PrimInt, Unsigned};

pub trait BitSlice {
    fn sel(&self, msb: usize, lsb: usize) -> Self;
    fn bit(&self, idx: usize) -> bool;
    fn mut_bit(&mut self, idx: usize, to: bool);
}

pub trait BitSlice64 {
    fn sel64(&self, msb: usize, lsb: usize) -> Self;
}

impl BitSlice64 for u64 {
    fn sel64(&self, msb: usize, lsb: usize) -> u64 {
        assert!(msb >= lsb, "invalid bit slice");
        let mask: u64 = if msb - lsb >= 63 {
            0xffffffffffffffffu64
        } else {
            (1u64 << (msb - lsb + 1)) - 1u64
        };
        (*self >> lsb) & mask
    }
}

impl<T: PrimInt + Unsigned + TryFrom<u64>> BitSlice for T {
    fn sel(&self, msb: usize, lsb: usize) -> T {
        let self64: u64 = self.to_u64().unwrap();
        self64.sel64(msb, lsb).try_into().map_err(|_| "").unwrap()
    }

    fn bit(&self, idx: usize) -> bool {
        let self64: u64 = self.to_u64().unwrap();
        self64.sel64(idx, idx) > 0
    }

    fn mut_bit(&mut self, idx: usize, to: bool) {
        let self64: u64 = self.to_u64().unwrap();
        *self = (self64 & (!(1u64 << idx)) | ((to as u64) << idx)).try_into().map_err(|_| "").unwrap();
    }
}

pub trait BitMask {
    fn to_u32(&self) -> u32;
    fn bitwise_or(&mut self, other: &Self);
}

impl BitMask for [bool] {
    fn to_u32(&self) -> u32 {
        assert!(self.len() <= 32, "Bitmask too large for u32");
        let mut mask_int = 0;
        for (i, bit) in self.iter().enumerate() {
            if *bit { 
                mask_int |= 1 << i; 
            }
        }

        mask_int
    }

    fn bitwise_or(&mut self, other: &Self) {
        assert_eq!(self.len(), other.len(), "Bitmasks must be of equal length");
        for (x, y) in self.iter_mut().zip(other.iter()) {
            *x = *x || *y;
        }
    }
}

impl BitMask for Vec<bool> {
    fn to_u32(&self) -> u32 {
        self.as_slice().to_u32()
    }

    fn bitwise_or(&mut self, other: &Self) {
        assert_eq!(self.len(), other.len(), "Vectors must be of equal length");
        for (x, y) in self.iter_mut().zip(other.iter()) {
            *x = *x || *y;
        }
    }
}

macro_rules! parse_val {
    ($value:expr) => {$value.parse().map_err(|_| format!("parse config value {} failed", $value))};
}
pub(crate) use parse_val;

macro_rules! for2d {
    ($matrix:expr, $closure:expr) => {
        for (i, row) in $matrix.iter().enumerate() {
            for (j, item) in row.iter().enumerate() {
                $closure(i as u32, j as u32, item);
            }
        }
    };
}
pub(crate) use for2d;

macro_rules! map2d {
    ($matrix:expr, $closure:expr) => {
        $matrix.iter().enumerate().map(|(i, row)| {
            row.iter().enumerate().map(|(j, item)| {
                $closure(i, j as u32, item)
            }).collect::<Vec<_>>()
        }).collect::<Vec<_>>()
    };
}
pub(crate) use map2d;

macro_rules! zip2d {
    ($matrix0:expr, matrix1:expr) => {
        $matrix0.iter().zip(matrix1).map(|(a, b)| {
            a.iter().zip(b)
        })
    };
}
pub(crate) use zip2d;

macro_rules! fill {
    ($value:expr, $n:expr) => {
        std::iter::repeat_with(|| $value).take($n).collect::<Vec<_>>()
    };
}
pub(crate) use fill;
