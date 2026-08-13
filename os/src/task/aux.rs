/// 传递给用户程序的 ELF auxiliary vector 条目类型。
///
/// 每个枚举值对应 Linux `elf.h` 中定义的 `AT_*` 常量。辅助向量由
/// 内核在创建用户栈时写入，运行时加载器可以据此获取程序头、页面大小、
/// 硬件特性以及随机数等启动信息。
#[derive(Clone, Copy)]
#[allow(non_camel_case_types, unused)]
#[repr(usize)]
#[derive(Debug)]
pub enum AuxType {
    /// 辅助向量结束标记。
    NULL = 0,
    /// 忽略该条目。
    IGNORE = 1,
    /// 传递可执行文件的文件描述符。
    EXECFD = 2,
    /// ELF 程序头表的用户空间地址。
    PHDR = 3,
    /// 单个 ELF 程序头的大小。
    PHENT = 4,
    /// ELF 程序头表中的条目数量。
    PHNUM = 5,
    /// 系统页面大小。
    PAGESZ = 6,
    /// ELF 解释器的装载基址。
    BASE = 7,
    /// ELF 文件标志。
    FLAGS = 8,
    /// 程序入口地址。
    ENTRY = 9,
    /// 表示文件不是 ELF 文件。
    NOTELF = 10,
    /// 实际用户 ID。
    UID = 11,
    /// 有效用户 ID。
    EUID = 12,
    /// 实际组 ID。
    GID = 13,
    /// 有效组 ID。
    EGID = 14,
    /// 目标平台名称字符串的地址。
    PLATFORM = 15,
    /// 处理器硬件能力位图。
    HWCAP = 16,
    /// 每秒时钟滴答数。
    CLKTCK = 17,
    /// x86 浮点控制字。
    FPUCW = 18,
    /// 数据缓存行大小。
    DCACHEBSIZE = 19,
    /// 指令缓存行大小。
    ICACHEBSIZE = 20,
    /// 统一缓存行大小。
    UCACHEBSIZE = 21,
    /// 已废弃的 PowerPC 专用条目。
    IGNOREPPC = 22,
    /// 程序是否以安全模式运行。
    SECURE = 23,
    /// 基础平台名称字符串的地址。
    BASE_PLATFORM = 24,
    /// 内核提供的随机数据地址。
    RANDOM = 25,
    /// 第二组处理器硬件能力位图。
    HWCAP2 = 26,
    /// 可执行文件名字符串的地址。
    EXECFN = 31,
    /// 系统调用入口地址（已废弃或架构相关）。
    SYSINFO = 32,
    /// `sysinfo` ELF header 的地址。
    SYSINFO_EHDR = 33,
    /// 一级指令缓存形状信息。
    L1I_CACHESHAPE = 34,
    /// 一级数据缓存形状信息。
    L1D_CACHESHAPE = 35,
    /// 二级缓存形状信息。
    L2_CACHESHAPE = 36,
    /// 三级缓存形状信息。
    L3_CACHESHAPE = 37,
    /// 一级指令缓存大小。
    L1I_CACHESIZE = 40,
    /// 一级指令缓存几何参数。
    L1I_CACHEGEOMETRY = 41,
    /// 一级数据缓存大小。
    L1D_CACHESIZE = 42,
    /// 一级数据缓存几何参数。
    L1D_CACHEGEOMETRY = 43,
    /// 二级缓存大小。
    L2_CACHESIZE = 44,
    /// 二级缓存几何参数。
    L2_CACHEGEOMETRY = 45,
    /// 三级缓存大小。
    L3_CACHESIZE = 46,
    /// 三级缓存几何参数。
    L3_CACHEGEOMETRY = 47,
    /// 最小信号栈大小。
    MINSIGSTKSZ = 51,
}

/// 一个 ELF auxiliary vector 条目。
#[derive(Debug)]
pub struct Aux {
    /// 条目的类型。
    pub aux_type: AuxType,
    /// 条目携带的机器字大小无符号值。
    pub value: usize,
}

impl Aux {
    /// 创建一个指定类型和值的辅助向量条目。
    pub fn new(aux_type: AuxType, value: usize) -> Self {
        Self { aux_type, value }
    }
}
