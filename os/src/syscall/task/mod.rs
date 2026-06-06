mod clone;
mod clone3;
mod ctl;
mod execve;
mod exit;
mod job;
mod kcmp;
mod keys;
mod schedule;
mod thread;
mod unshare;
mod wait;
mod acct;
mod resource;

pub use self::{
    clone::*, clone3::*, ctl::*, execve::*, exit::*, job::*, keys::*, schedule::*, thread::*,
    unshare::*, wait::*, acct::*, resource::*, kcmp::*,
};
