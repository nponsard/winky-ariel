#![no_std]

use tic_parser::Report;

#[repr(C)]
pub struct Control {
    reports: [Report; 100],
}
