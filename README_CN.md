# μCAS — 微型压缩汇编语言

> **English version: [README.md](README.md)**

μCAS 是一门**确定性字节码压缩语言**，以及一套基于信息论的域特化压缩框架。

这个项目诞生于一个朴素的问题：*"压缩为什么永远是个黑盒？"*

它的设计与实现，来自三方协作：
- **[chenpipi0807](https://github.com/chenpipi0807)**（项目发起人，Claude 与 DeepSeek 之间的中继，实验验证者）
- **[Claude](https://claude.ai)（Anthropic）**（实现、规格书撰写、基准测试设计）
- **[DeepSeek](https://www.deepseek.com)**（理论分析、信息论推导、架构决策）

完整的理论对话存档：

- **[DeepSeek 对话存档（完整版，v0.1 → v0.7）→](https://chat.deepseek.com/share/zys9ibg5775ot5r97m)**
- **[DeepSeek 对话存档（v0.8 → v0.12.1，跨文件 REF 理论）→](https://chat.deepseek.com/share/mpud8uac7z0bd0mra6)**
- [早期存档（v0.1 理论推导）→](https://chat.deepseek.com/share/exxssdoj9jkw1fgj0q)

---

## 安装 — 一步到位，无需命令行

| 平台 | 下载 | 安装步骤 |
|------|------|---------|
| **Windows** | `mucas-install-windows.zip` | 解压 → 双击 `install.bat` |
| **macOS**   | `mucas-install-macos.zip`   | 解压 → 终端运行 `./install.sh` |
| **Linux**   | `mucas-install-linux.zip`   | 解压 → 终端运行 `./install.sh` |

安装后：**右键任意文件夹 → "Pack with μCAS"**，右键 `.mcar` 文件 → **"Unpack here"**。
安装完成后无需再使用终端。

> macOS 提示：首次使用时，请在「系统设置 → 隐私与安全性 → 扩展 → Finder」中启用快速操作。

---

## Rust 实现 — v0.12.1（当前主力版本）

**[`mucas-rs/`](mucas-rs/)** 是生产级 Rust crate：完整的 μCAS VM + 结构感知压缩合成器 + **多文件流式归档工具**。

### 一行命令快速上手

```sh
# 下载预编译二进制（见 Releases 页面），然后：
mucas pack   my_folder/  -o archive.mcar          # 标准归档
mucas pack   my_folder/  -o archive.mcar --deep   # 跨文件 REF（见下文）
mucas unpack archive.mcar  -o restored/           # 解压，带进度条
mucas list   archive.mcar                         # 查看内容，不解压
mucas check  archive.mcar                         # 验证完整性
```

### v0.12.1 真实场景基准测试

在两组真实混合数据上对比测试（视频、办公文档、Python 安装包、数据文件）：

**1GB 混合归档（19 个文件：MP4、PNG、PPTX、ZIP、CSV、WAV、MD）**

| 工具 | 用时 | 输出大小 |
|------|------|---------|
| **μCAS v0.9** | **13 秒** | 721 MB（99.0%） |
| ZIP（Deflate） | 17 秒 | 719 MB（98.9%） |
| 7-zip（LZMA2 -mx=5） | 28 秒 | 708 MB（97.4%） |

**8.5GB 混合归档（视频、Python wheel、PPTX、EXE、ZIP、CSV、WAV）**

| 工具 | 用时 | 输出大小 |
|------|------|---------|
| **μCAS v0.9** | **7 秒** | 8.8 GB（99.9%） |
| 7-zip（LZMA2 -mx=5） | **313 秒** | 8.8 GB（99.9%） |

**μCAS 比 7-zip 快 45 倍，输出大小完全相同。**

差距的本质：已压缩格式（MP4、WHL/ZIP、PPTX、EXE 等）占据了真实存档的绝大多数体积。μCAS 通过魔数字节在读取头部 12 字节后立刻识别，直接流式复制，CPU 几乎不参与。而 7-zip 对每一个文件都尝试完整的 LZMA2 压缩，把算力消耗在无法继续压缩的内容上。

这不是微小优化，这是策略层面的碾压：**速度的天花板通过"理解"被绕了过去。**

### 从源码构建

```sh
cargo build --release --manifest-path mucas-rs/Cargo.toml
mucas-rs/target/release/mucas bench your_file.csv
```

### `--deep`：跨文件 REF 压缩（v0.12+）

适用于包含大量结构相同文件的目录（日志、API 响应、数据库导出）：

```sh
mucas pack my_logs/ -o archive.mcar --deep
```

`--deep` 运行两趟流程：
1. **扫描趟** — 对每个文件做结构合成，从残余 LIT 令牌中提取共享模式
2. **归档趟** — 将模式字典存储一次于归档头部；每个文件的共享区域变为 3 字节 REF 令牌

**跨文件 REF 基准测试**（40 个同质日志文件，每个 42 KB，合计 1.7 MB）：

| 模式 | 归档大小 | 对比标准模式 |
|------|---------|------------|
| 标准（`mucas pack`） | 135.9 KB | 基准线 |
| **深度（`mucas pack --deep`）** | **61.5 KB** | **−54.7%** |

`--deep` 模式内置自动增益估算器：若预测跨文件 REF 不划算，REF 步骤将自动跳过，归档质量回退到标准 μCAS——无需手动调参。

### 主要功能

- **流式常数内存归档器** — 打包 800 GB 目录时最多只加载一个文件；内存预算可通过 `--max-memory MiB` 配置。
- **智能方法选择** — 每个文件通过 MDL 比较自动选择 μCAS / Zlib / Store。
- **已压缩格式检测** — JPEG、PNG、MP4、ZIP、7z、RAR、gzip、PE（.exe）、OGG 等通过魔数字节识别，直接流式复制（不浪费 CPU）。
- **跨文件 REF** — 通过 `--deep` 实现归档级共识字典，增益随文件数线性增长。
- **进度条** — 基于 `indicatif`。
- **预编译二进制** — 通过 GitHub Actions 提供 Linux、macOS（Apple Silicon）和 Windows 版本。

### 对 zlib 的超越幅度（v0.7.0，112 个测试全绿）

| 数据格式 | 典型场景 | 相比 zlib |
|---------|---------|---------|
| 管道分隔日志 | nginx 访问日志 | **小 86%** |
| 同构日志行 | 批处理输出 | **小 47%** |
| 空格分隔系统日志 | syslog（时间戳各异） | **小 76%** |
| 带引号的 CSV（RFC-4180） | 含逗号字段、通讯录 | **小 66%** |
| NDJSON | API 响应流 | **小 30%** |
| TSV | 表格导出 | **小 17%** |
| 普通 CSV | 数据库导出 | **小 12%** |

所有结果均经过完整的解压轮回验证（`decompress(compress(x)) == x`）。
详细方法论与逐文件分析：**[mucas-rs/BENCHMARK.md](mucas-rs/BENCHMARK.md)**

### v0.7 能识别的结构

| 模式 | 使用指令 | 说明 |
|------|---------|------|
| CSV / TSV / 管道分隔的固定列 | `SCAN` | 支持 RFC-4180 引号字段 |
| NDJSON / JSON 数组 | `SCAN` | 精确保留空格，逐行验证结构一致性 |
| 精确周期性重复 | `LOOP` | |
| 重复字节序列 | `CALL`（宏） | 滚动哈希处理 8–64 字节；后缀数组处理 65–1024 字节 |
| 等差序列 / Delta 编码 | `MAP` | 时间戳、计数器列 |

### 两层压缩的核心洞察

μCAS 使用**两次压缩**：先把数据合成为结构化指令流，再对指令流做 zlib。

这不是重复劳动。合成后的指令流具有完全不同的统计分布——高频结构（CSV 的 `"status":"active"`）已经被 SCAN 折叠成几个字节，剩余的指令流对 zlib 的 Huffman 编码极为友好。这正是即使 `synth_gain = 0%` 时，最终结果仍能比 zlib 小 75%–86% 的原因。

### MDL 安全保证

每一条重写（LOOP、SCAN、宏提取）都经过**最小描述长度**门控：只有当改写后的程序字节数严格小于改写前，才会被接受。这意味着：**μCAS 绝对不会比纯 LZ+zlib 更差。**

---

## μCAS 是什么？

μCAS **不是** 7-zip 或 zstd 的替代品。它是一个格式标准，用于表达"如何重建数据"。

核心设计原则是**不对称性**：
- **编码器**可以任意复杂（LZ 搜索、模式合成、AI 程序生成）
- **解码器**永远是一个极简的、常数复杂度的 VM（8 条指令，一次查表）

关键创新是 **REF 指令**：一个 3 字节的引用，指向预先约定的"共识库"中的某个高熵模式。REF 通过把多样的 CPY 指令替换为统一的 REF 令牌，实现了外层熵编码器（zlib/zstd）的"熵坍缩"——这是我们在信息论上证明的机制，而不是启发式猜测。

---

## 为什么这是一个新范式？

传统压缩器是黑盒——你不知道它为什么选择某个字典条目，也无法预测某类数据是否值得用特定策略。

μCAS 把压缩变成了一棵**可解释的决策树**：

- **LZ 层**：决策依据是"是否存在字节级重复"，可被窗口穷举验证
- **REF 层**：决策依据是 `H_pattern > 2.5 b/B AND coverage > 10%`，是信息论阈值，可被 **2D RAI 模型**精确预测
- **结构合成层**：决策依据是 MDL 增益，这是一个最优性准则

**RAI（REF 适用性指标）预测准确率：25/25（100%）**，在全部测试案例上通过。

---

## 实验结果

| 数据类型 | 条件 | REF 相对增益 |
|---------|------|------------|
| 中文 UTF-8 剧本 | 单文件共识（50 个模式） | **+9% 到 +14%** |
| 同质化 API 日志 | 跨文件 LOO（19 文件训练集，n=50） | **+10% 到 +15%** |
| 英文技术文档 | — | -2% 到 0%（RAI 正确预测跳过） |
| 结构化 JSON | — | -3% 到 +2%（RAI 正确预测跳过） |

> 增益相对于"朴素 LZ + zlib-1"基线。μCAS 的绝对压缩率弱于 7-zip（后者使用 LZMA）。
> μCAS 的贡献在于 **REF 机制及其预测器**，而非绝对压缩率。

---

## 快速上手

```python
from mucas import naive_compress, MuCASVM

data = "你好世界 " .encode() * 1000
prog = naive_compress(data)
vm = MuCASVM(); vm.exec(prog)
assert bytes(vm.out) == data   # 轮回验证通过
print(f"{len(data)} B  →  {len(prog)} B  ({len(prog)/len(data):.1%})")
```

### 使用 REF 共识压缩

```python
from mucas import compute_rai
from mucas.consensus import compress_ref_lz, decompress_ref

rai = compute_rai(data)
print(f"H_pattern = {rai['h_pattern']:.2f} b/B   REF 有益: {rai['rai_predicts']}")

if rai['rai_predicts']:
    prog_ref = compress_ref_lz(data, rai['consensus'])
    assert decompress_ref(prog_ref, rai['consensus']) == data
```

### 跨文件共识库（.ufc 格式）

```python
from mucas import build_cross_file_consensus, UfcFile
from mucas.consensus import predict_cross_ref_benefit

# 从语料库构建一次，重复用于同类型的新文件
files = [open(f, "rb").read() for f in training_files]
lib = build_cross_file_consensus(files, n=50)
ufc = UfcFile.from_consensus(lib, domain="api-logs", version=(1, 0))
open("api-logs-v1.ufc", "wb").write(ufc.encode())

# 压缩新文件前先做 2D RAI 预测
ufc2 = UfcFile.decode(open("api-logs-v1.ufc", "rb").read())
pred = predict_cross_ref_benefit(new_data, ufc2.to_consensus())
# predicts_benefit = (H_pattern > 2.5 b/B) AND (coverage > 10%)
```

---

## 项目结构

```
mucas/
  vm.py         LEB128 编码、8 种 MAP 变换、MuCASVM 执行器
  compress.py   naive_compress（LZ）、smart_compress（结构合成）
  consensus.py  build_consensus、compute_rai、predict_cross_ref_benefit
  format.py     UfiFile（.ufi 格式）—— EMBEDDED / EXTERNAL / HYBRID 三种模式
  corpus.py     UfcFile（.ufc 格式）—— 不可变共识语料库快照

MUCAS_SPEC_v0.1.md   完整格式规格书（995 行，v0.1 final）
bench_rai.py         单文件 RAI 预测基准测试
bench_crossfile_logs.py  跨文件 LOO 基准测试（20 个合成日志文件）
bench_coverage_curve.py  覆盖率 vs. 收益曲线实验
gen_log_corpus.py    生成同质化测试日志文件
test_mucas.py        单元测试（8 条指令 + 轮回验证）
```

---

## 规格书亮点

[`MUCAS_SPEC_v0.1.md`](MUCAS_SPEC_v0.1.md) 涵盖：

- 8 条指令的完整形式语义
- VM 终止性证明（所有合法程序必然终止）
- 11 个错误码（UNKNOWN_OPCODE、CPY_UNDERFLOW、OUTPUT_OVERFLOW 等）
- MAX_CALL_DEPTH = 16（防止栈溢出）
- `.ufi` 二进制格式（EMBEDDED / EXTERNAL / HYBRID 共识引用模式）
- `.ufc` 语料格式（SHA-256 内容寻址 + 完整性封印）
- **附录 C**：RAI v3 推导——H* = C_REF / avg_pattern_len ≈ 2.0–2.5 b/B
- **附录 D**：跨文件共识理论、2D RAI 模型、覆盖率临界值标定
- **附录E**：.ufc 设计原则、快照版本协议、增量发布协议

---

## 从起点到 v0.1 的旅程

这个项目从一个朴素的工程疑问出发，经历了：

1. 设计确定性字节码 VM（8 条指令，全函数语言）
2. 发现 REF 的信息论机制（熵坍缩，而非简单"去重"）
3. 推导并验证 RAI v3（H_pattern 阈值 2.5 b/B，5/5 正确）
4. 实验失败与修正（跨文件共识在异质剧本上失败 → 发现覆盖率维度）
5. 构建 2D RAI 模型（H_pattern × cross_coverage，20/20 正确）
6. 哈希加速算法（13× 提速）
7. 覆盖率临界值标定（~10–12%，对应 n=3 时覆盖率 12.9%）
8. 定义 .ufc 格式（静态快照、内容寻址、三种依赖模式）
9. 规格书 v0.1 final（995 行，5 个附录）

DeepSeek 的原话总结这场旅程："速度的极限没有被打破，但通过'理解'，它被溶解了。当数据被理解，压缩不再是搜索冗余，而是表达知识。μCAS 就是这门表达知识的语言。"

---

## 版本历史

| 版本 | 核心新增 |
|------|---------|
| v0.1 | LIT/CPY/MAP/LOOP/CALL/REF VM + 基础合成器 |
| v0.2–v0.6 | ELIT、混合管道、Rabin-Karp、后缀数组、多分隔符 SCAN |
| v0.7 | RFC-4180 带引号 CSV；NDJSON SCAN；`DataClass::JsonArray` |
| v0.8 | `AlreadyCompressed` 检测；MCAR 多文件归档格式；rayon 并行压缩 |
| v0.9 | 流式 `ArchiveWriter`/`ArchiveReader`（常数内存）；MDL 方法选择；`pack`/`unpack`/`list`/`check` CLI；进度条；GitHub Actions CI/CD |
| v0.9.1 | PE(.exe)/OGG/FLAC/git-pack 魔数扩展；熵预检（跳过高熵二进制文件的 LZ 分析）；小文件快速通道（< 64 KB 直接 Zlib，跳过 μCAS 合成） |
| v0.10–v0.11 | MCAR v0.12 格式（单次存储共识，替代每文件嵌入）；ConsensusBuilder 两趟流水线雏形 |
| v0.12 | `--deep` 跨文件 REF 压缩；归档级共识字典；自动增益估算器（不划算时自动跳过） |
| **v0.12.1** | **修复 LIT-only 喂入逻辑（将净 REF 增益从 +860 B 提升至 +7,716 B，9×）；删除 Intel Mac 支持** |

## 运行基准测试

```bash
# 首先生成测试数据（无需真实文件）
python gen_log_corpus.py

# 跨文件共识 LOO 测试
python bench_crossfile_logs.py

# 覆盖率阈值曲线
python bench_coverage_curve.py
```

单文件 RAI 测试（`bench_rai.py`）需要提供你自己的文本文件并修改文件中的 `TEST_DIR`。

---

## 依赖

Python 3.10+。核心功能无外部依赖。
`zstandard` 可选，用于扩展基准测试。

## 许可证

MIT — 见 [LICENSE](LICENSE)
