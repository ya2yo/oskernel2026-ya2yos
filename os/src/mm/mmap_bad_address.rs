use hashbrown::HashSet;
use spin::{lazy::Lazy, Mutex};

//坏地址表，mmap映射坏地址时加入此表
static BAD_ADDRESS: Lazy<Mutex<HashSet<usize>>> = Lazy::new(|| Mutex::new(HashSet::new()));

pub fn insert_bad_address(va: usize) {
    BAD_ADDRESS.lock().insert(va);
}

pub fn if_bad_address(va: usize) -> bool {
    BAD_ADDRESS.lock().contains(&va)
}

pub fn remove_bad_address(va: usize) {
    BAD_ADDRESS.lock().remove(&va);
}
