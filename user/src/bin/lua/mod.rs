// lua_testcode:
// ./busybox echo "#### OS COMP TEST GROUP START lua-musl ####"
// "date.lua
// "file_io.lua
// "max_min.lua
// "random.lua
// "remove.lua
// "round_num.lua
// "sin30.lua
// "sort.lua
// "strings.lua
// ./busybox echo "#### OS COMP TEST GROUP END lua-musl ####"

// test.sh:
// #!/bin/busybox sh

// ./lua $1
// if [ $? == 0 ]; then
// 	echo "testcase lua $1 success"
// else
// 	echo "testcase lua $1 fail"
// fi

use crate::*;
#[allow(unused)]
pub fn run_lua_musl_testsuit(path: &str) {
    let r = fork_and_run("/musl\0", &["./lua\0", path]);
    if r == 0 {
        println!("testcase lua {} success", path);
    } else {
        println!("testcase lua {} fail", path);
    }
}

/// loongarch-lua-glibc还不行……
#[allow(unused)]
pub fn run_lua_glibc_testsuit(path: &str) {
    let r = fork_and_run("/glibc\0", &["./lua\0", path]);
    if r == 0 {
        println!("testcase lua {} success", path);
    } else {
        println!("testcase lua {} fail", path);
    }
}
#[allow(unused)]
pub static ALL_LUA: [&str; 9] = [
    "date.lua\0",
    "file_io.lua\0",
    "max_min.lua\0",
    "random.lua\0",
    "remove.lua\0",
    "round_num.lua\0",
    "sin30.lua\0",
    "sort.lua\0",
    "strings.lua\0",
];
#[allow(unused)]
pub static LUA_BLACKLIST: [&str; 1] = ["date.lua\0"];
#[allow(unused)]
pub fn run_all_lua_musl() {
    println!("#### OS COMP TEST GROUP START lua-musl ####");
    for i in ALL_LUA {
        if !LUA_BLACKLIST.contains(&i) {
            run_lua_musl_testsuit(i);
        }
    }
    println!("#### OS COMP TEST GROUP END lua-musl ####")
}

/// loongarch-lua-glibc还不行……
#[allow(unused)]
pub fn run_all_lua_glibc() {
    println!("#### OS COMP TEST GROUP START lua-glibc ####");
    for i in ALL_LUA {
        if !LUA_BLACKLIST.contains(&i) {
            run_lua_glibc_testsuit(i);
        }
    }
    println!("#### OS COMP TEST GROUP END lua-glibc ####")
}
