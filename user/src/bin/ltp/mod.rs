use crate::*;
mod filelist;
pub use filelist::FILELIST;
#[allow(unused)]
pub fn run_ltp_tests_musl(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    for &test in tests {
        if blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);
        
        let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-musl ####");
}
#[allow(unused)]
pub fn check_ltp_tests_musl(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    for &test in tests {
        if !blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);
        
        let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-musl ####");
}
#[allow(unused)]
pub fn run_ltp_tests_glibc(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-glibc ####");
    for &test in tests {
        if blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);
        let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-glibc ####");
}
