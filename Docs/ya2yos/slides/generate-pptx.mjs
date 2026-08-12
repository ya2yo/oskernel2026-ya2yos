// Dependency-free PPTX generator for the Ya2yOS defense outline.
// Run: node Docs/ya2yos/slides/generate-pptx.mjs
import { mkdtempSync, rmSync, writeFileSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const output = "Docs/ya2yos/slides/ya2yos-defense.pptx";
const slides = [
  ["Ya2yOS", "从能启动到能承载真实负载", "Rust 宏内核 · Linux 用户态兼容 · RISC-V64 / LoongArch64", "2 个目标架构　·　4 条近月工程主线　·　1.50x 定向 BuildStorm 加速", "参赛队员：饶晓杰　指导老师：杨磊", "代码快照：HEAD 088f10b82bb8 · 2026-08-12"],
  ["01 · 项目定位", "我们解决什么问题？", "让真实 Linux 用户态路径在双架构 Rust 内核上闭环运行", "用户态目标：glibc / musl、BusyBox、libc-test、LTP 子集与 Rust 工具链", "内核形态：Rust 宏内核；以 Linux ABI 为兼容边界，而不是 syscall 数量竞赛", "工程约束：高并发 fork/exec/mmap、真实 ext4、SMP 共享地址空间、QEMU 双架构", "近月工作的重点：把“能调用”推进到“语义正确、并发可控、证据可复核”。"],
  ["02 · 总体架构", "一条用户态请求如何穿过内核？", "Cargo / Rustc / BusyBox / LTP", "syscall · trap · uaccess · ABI 校验", "task / mm / fs / net / signal / timer", "VFS + lwext4 + page cache + VirtIO + smoltcp", "核心设计原则：syscall 入口保持薄；语义落在所属子系统；锁外 I/O、锁内短更新。"],
  ["03 · 近月主线 A", "共享地址空间：把一致性协议做成可解释的状态机", "问题：多 hart 同时执行 fork / COW / munmap / mremap，逐目标等待、全量帧保留和旧 TLB 可能放大延迟与错误。", "方案：范围化旧帧保留；广播 mailbox 后收集 ACK；ACK 收敛前不释放旧帧；独占 COW 就地恢复写权限。", "UPDATE_LOCK → MemorySet 写锁 → PTE 更新 → TLB / I-cache shootdown → 释放旧帧", "RISC-V Sv39 / LoongArch PTE + IBar / 同一生命周期协议"],
  ["04 · 近月主线 B", "从信号帧到动态链接：边界回归 Linux 语义", "信号 ABI：rt_sigreturn 读取完整受检 frame；非法 frame 返回 EINVAL；两架构统一布局与 trampoline。", "动态 ELF：PT_INTERP 原路径经 VFS 打开；缺失保留 ENOENT；共享对象返回原始字节。", "新增兼容面：memfd_secret 基础匿名 fd、signalfd4、System V 消息队列、rseq、seccomp 等。", "关键取舍：内核负责 ELF/VFS 边界；库搜索、重定位和安全增强不伪装成内核已实现。"],
  ["05 · 近月主线 C", "ext4 并发优化：缩短安全串行域，而不是取消它", "必须保留的边界：lwext4 C API、journal、bcache callback、单一 VirtIO 队列存在不可并行资源；任务感知锁负责 park/wake 与退出清理。", "可安全消除的重复：页缓存复用、连续冷页 read、目录局部 stat epoch、稀疏 range 合并、连续 bcache 写回批处理。", "VFS 短锁 · 资源锁 FIFO · 锁外 I/O · 失败可重试"],
  ["06 · 近月主线 D", "调度与时间：让多核真实可见、让计时可信", "共享 CFS：all-hart ready queue、affinity 过滤、空闲远端 hart IPI 唤醒。", "全局 timer：每 10ms 由单一 hart 排他维护共享 timer/futex 状态；当前线程 interval timer 仍在本 hart 投递。", "可观测性：/proc/uptime 动态生成；perf 按 scheduler / COW / TLB / EXT4 / block 聚合，release 默认低开销。", "时间口径先正确，性能数字才有意义。累计计数用于定位，不冒充 wall-clock。"],
  ["07 · 性能证据", "BuildStorm：从“跑不通”进入“可优化”", "可追溯定向观测：同一尾部编译单元 12 min → 8 min，时间缩短约 33.3%，加速约 1.50x。", "证据边界：当前日志没有官方完整 BUILDSTORM_COMPILE ... ok=true elapsed_s=... 收尾行，不宣称 446 crate 全量成绩。", "正确性 → 可观测性 → 热点归因 → 局部优化 → 双架构回归"],
  ["08 · 验证方法", "我们如何知道改动没有破坏内核？", "语义证据：TPASS / TFAIL / TBROK、panic、errno 与 summary；关注测试断言而非包装脚本返回码。", "架构证据：make TARGET_ARCH=riscv64 与 loongarch64；共享状态改动尽量双侧构建/运行。", "设计证据：problem 复盘保留背景、现象、根因、修复、涉及文件、验证与已知边界。", "当前快照：HEAD 088f10b82bb8 · 文档版本 0.6"],
  ["09 · 设计选择", "我们刻意没有把什么写成“已经完成”？", "明确边界：完整 io_uring、timerfd、独立 procfs/devtmpfs、真实 loop backing file、网络中断唤醒、NUMA、完整 namespace/cgroup/LSM 仍是后续方向。", "答辩口径：把“基础兼容 fd”与“完整 Linux 安全语义”分开；把“定向加速”与“完整成绩”分开；把“接口存在”与“测试通过”分开。", "诚实的边界描述，本身是内核设计可维护性的组成部分。"],
  ["10 · 下一步", "从兼容面走向更深的系统语义", "短期：完善脏页回写调度、VirtIO-net 中断/唤醒、真实 loop 数据路径、stub 分类与双架构自动化矩阵。", "中期：独立 tmpfs/procfs/devtmpfs、mount namespace 与真实 dentry 切换、CPU affinity/负载均衡、io_uring/AIO。", "长期：namespace/cgroup/capability/seccomp 深化、更多 VirtIO 设备、IPv6/netlink/raw socket 与工程化持续测试。", "主线：减少兼容假设，把已经跑通的路径做深。"],
  ["Ya2yOS", "谢谢！", "问题与讨论", "设计文档：Docs/ya2yos/　·　问题复盘：Docs/决赛文档/problem/"]
];

const esc = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
const write = (root, path, data) => { const file = join(root, path); mkdirSync(join(file, ".."), { recursive: true }); writeFileSync(file, data); };
const crcTable = Uint32Array.from({ length: 256 }, (_, n) => {
  let value = n;
  for (let bit = 0; bit < 8; bit += 1) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value >>> 0;
});
const crc32 = (data) => {
  let value = 0xffffffff;
  for (const byte of data) value = crcTable[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
};
const walkFiles = (root, relative = "") => readdirSync(join(root, relative)).flatMap((name) => {
  const entry = relative ? `${relative}/${name}` : name;
  return statSync(join(root, entry)).isDirectory() ? walkFiles(root, entry) : [entry];
});
const u16 = (value) => { const out = Buffer.alloc(2); out.writeUInt16LE(value); return out; };
const u32 = (value) => { const out = Buffer.alloc(4); out.writeUInt32LE(value >>> 0); return out; };
const createZip = (root, destination) => {
  let offset = 0;
  const locals = [];
  const central = [];
  for (const path of walkFiles(root)) {
    const name = Buffer.from(path);
    const data = readFileSync(join(root, path));
    const crc = crc32(data);
    const local = Buffer.concat([u32(0x04034b50), u16(20), u16(0x0800), u16(0), u16(0), u16(0), u32(crc), u32(data.length), u32(data.length), u16(name.length), u16(0), name, data]);
    locals.push(local);
    central.push(Buffer.concat([u32(0x02014b50), u16(20), u16(20), u16(0x0800), u16(0), u16(0), u16(0), u32(crc), u32(data.length), u32(data.length), u16(name.length), u16(0), u16(0), u16(0), u16(0), u32(0), u32(offset), name]));
    offset += local.length;
  }
  const centralData = Buffer.concat(central);
  const end = Buffer.concat([u32(0x06054b50), u16(0), u16(0), u16(central.length), u16(central.length), u32(centralData.length), u32(offset), u16(0)]);
  writeFileSync(destination, Buffer.concat([...locals, centralData, end]));
};
const textShape = (id, text, x, y, cx, cy, size, color, bold = false) => `<p:sp><p:nvSpPr><p:cNvPr id="${id}" name="Text ${id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="${x}" y="${y}"/><a:ext cx="${cx}" cy="${cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr wrap="square" lIns="38100" rIns="38100" tIns="25400" bIns="25400"/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN" sz="${size}" b="${bold ? 1 : 0}" solidFill="${color}" typeface="Aptos"/><a:t>${esc(text)}</a:t></a:r><a:endParaRPr lang="zh-CN"/></a:p></p:txBody></p:sp>`;
const slideXml = (items, index) => {
  const shapes = [textShape(2, items[0], 457200, 228600, 10972800, 381000, 2000, "1677B8", true), textShape(3, items[1], 457200, 635000, 10972800, 635000, 3400, "102A43", true)];
  let y = 1428750;
  items.slice(2).forEach((item, n) => { shapes.push(textShape(4 + n, item, 609600, y, 10363200, 508000, n === 0 ? 1750 : 1450, n === 0 ? "52606D" : "1D2833", n === 0)); y += n === 0 ? 711200 : 660400; });
  shapes.push(textShape(20, `Ya2yOS · 2026-08 · ${index}`, 457200, 6654800, 10972800, 177800, 900, "52606D"));
  return `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id="0" name="Background"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12192000" cy="6858000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody></p:sp>${shapes.join("")}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>`;
};
const root = mkdtempSync(join(tmpdir(), "ya2yos-pptx-"));
try {
  write(root, "[Content_Types].xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>${slides.map((_, i) => `<Override PartName="/ppt/slides/slide${i + 1}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>`).join("")}</Types>`);
  write(root, "_rels/.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>`);
  write(root, "docProps/core.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Ya2yOS 内核设计与工程实践答辩</dc:title><dc:creator>Ya2yOS</dc:creator><dcterms:created xsi:type="dcterms:W3CDTF">2026-08-12T00:00:00Z</dcterms:created></cp:coreProperties>`);
  write(root, "docProps/app.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Ya2yOS dependency-free PPTX generator</Application><Slides>${slides.length}</Slides></Properties>`);
  write(root, "ppt/presentation.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>${slides.map((_, i) => `<p:sldId id="${256 + i}" r:id="rId${i + 2}"/>`).join("")}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000" type="screen16x9"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>`);
  write(root, "ppt/_rels/presentation.xml.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>${slides.map((_, i) => `<Relationship Id="rId${i + 2}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide${i + 1}.xml"/>`).join("")}</Relationships>`);
  write(root, "ppt/slideMasters/slideMaster1.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name=""><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>`);
  write(root, "ppt/slideMasters/_rels/slideMaster1.xml.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>`);
  write(root, "ppt/slideLayouts/slideLayout1.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank" preserve="1"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld></p:sldLayout>`);
  write(root, "ppt/slideLayouts/_rels/slideLayout1.xml.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>`);
  slides.forEach((slide, i) => { write(root, `ppt/slides/slide${i + 1}.xml`, slideXml(slide, i + 1)); write(root, `ppt/slides/_rels/slide${i + 1}.xml.rels`, `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>`); });
  createZip(root, join(process.cwd(), output));
  process.stdout.write(`Generated ${output} (${slides.length} slides).\n`);
} finally { rmSync(root, { recursive: true, force: true }); }
