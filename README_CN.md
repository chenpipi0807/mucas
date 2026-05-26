# μCAS — 微型压缩汇编语言

> **English version: [README.md](README.md)**

μCAS 是一门**确定性字节码压缩语言**，以及一套基于信息论的域特化压缩框架。

这个项目诞生于一个朴素的问题：*"压缩为什么永远是个黑盒？"*

它的设计与实现，来自三方协作：
- **用户**（项目发起人，Claude 与 DeepSeek 之间的中继，实验验证者）
- **[Claude](https://claude.ai)（Anthropic）**（实现、规格书撰写、基准测试设计）
- **[DeepSeek](https://www.deepseek.com)**（理论分析、信息论推导、架构决策）

完整的理论对话存档：

- **[DeepSeek 对话存档（完整版，v0.1 → v0.7）→](https://chat.deepseek.com/share/zys9ibg5775ot5r97m)**
- [早期存档（v0.1 理论推导）→](https://chat.deepseek.com/share/exxssdoj9jkw1fgj0q)

---

## Rust 实现 — v0.7.0（当前主力版本）

**[`mucas-rs/`](mucas-rs/)** 是生产级 Rust crate：完整的 μCAS VM + 结构感知压缩合成器 + CLI 工具。

```sh
cargo build --release --manifest-path mucas-rs/Cargo.toml
mucas-rs/target/release/mucas bench your_file.csv
```

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
