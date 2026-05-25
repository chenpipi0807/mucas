"""
Generate synthetic homogeneous log files for cross-file consensus testing.

Each file simulates a different API key test session with the same program,
mimicking the real repro_log_SVIP.txt / repro_log_poly_spark.txt format.
Fixed structure + variable content = ideal cross-file consensus test bed.
"""
import os, random, hashlib, struct, time

OUT_DIR = r"d:\P-ZIP\test\log_corpus"
os.makedirs(OUT_DIR, exist_ok=True)

random.seed(42)

ENDPOINTS = [
    "https://openproxy-cn.zuoyebang.cc/openproxy/rp/doubao/v3/contents/generations/tasks",
    "https://openproxy-cn.zuoyebang.cc/openproxy/rp/doubao/v3/contents/generations/tasks",
]
MODEL = "doubao-seedance-2-0-260128"
BASE_URL = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAZAAAAGQCAIAAA"

SCENE_TEMPLATES = [
    "{loc}，{char1}站于画面左侧，{char2}立于画面右侧，{shot}，{light}",
    "{loc}，镜头{motion}至{char1}面部特写，他眼神{gaze}",
    "慢动作跟拍{obj}出手，{obj}弧划过",
    "镜头切至{char2}，他抬头看向{char1}，嘴角微动",
    "镜头拉远至两人同框，画面渐稳收束",
]

LOCS = ["篮球场中景", "教室内景", "操场全景", "走廊近景", "食堂中景", "图书馆内景"]
CHARS = ["蛐蛐", "雷达", "小明", "阿杰", "志远", "思远", "子豪", "阿辉"]
SHOTS = ["镜头低角度仰拍", "俯拍全景", "平拍中景", "侧面跟拍"]
LIGHTS = ["午后阳光斜射", "黄昏暖光", "清晨晨光", "室内日光灯"]
MOTIONS = ["推近", "拉近", "平移", "旋转"]
GAZES = ["锁定篮筐", "望向远处", "低垂", "扫视全场"]
OBJS = ["篮球", "足球", "纸飞机", "石子"]

RESPONSE_TEMPLATES = [
    '[{ts}] 响应状态码: 200',
    '[{ts}] task_id: {tid}',
    '[{ts}] 轮询 #{n}: status=processing, progress={prog}%',
    '[{ts}] 轮询 #{n}: status=processing, progress={prog}%',
    '[{ts}] 轮询 #{n}: status=processing, progress={prog}%',
    '[{ts}] 轮询 #{n}: status=success',
    '[{ts}] 视频下载完成, size={size}KB',
    '[{ts}] === 测试完成 ===',
]

FORBIDDEN = "画面中严格禁止出现任何形式的字幕、文字、字母、数字、符号、水印、标题。"
SEP = "=" * 78


def rand_ts(h: int, m: int, s: int, ms: int) -> str:
    s += random.randint(0, 2)
    ms = random.randint(0, 999)
    return f"[{h:02d}:{m:02d}:{s:02d}.{ms:03d}]"


def rand_tid() -> str:
    return hashlib.md5(random.randbytes(8)).hexdigest()


def gen_scene_desc() -> str:
    c1, c2 = random.sample(CHARS, 2)
    lines = []
    t = 0
    while t < 10:
        dur = random.choice([2, 2, 4])
        tmpl = random.choice(SCENE_TEMPLATES)
        desc = tmpl.format(
            loc=random.choice(LOCS), char1=c1, char2=c2,
            shot=random.choice(SHOTS), light=random.choice(LIGHTS),
            motion=random.choice(MOTIONS), gaze=random.choice(GAZES),
            obj=random.choice(OBJS),
        )
        lines.append(f"{t}-{t+dur}s {desc}")
        t += dur
    lines.append(FORBIDDEN)
    return "\n".join(lines)


def gen_log(key_name: str, key_val: str, h: int, m: int) -> str:
    ts0 = rand_ts(h, m, 0, 0)
    s = m * 60

    lines = [
        f"{ts0} === 开始测试 key = {key_name} ===",
        f"{ts0} key 值: {key_val}",
        f"{ts0} endpoint: {ENDPOINTS[0]}",
        f"{ts0} model: {MODEL}",
        "",
        SEP,
        "POST 请求体 (base64 已截短)",
        SEP,
        "{",
        f'  "model": "{MODEL}",',
        '  "content": [',
        "    {",
        '      "type": "text",',
        '      "text": "' + gen_scene_desc().replace('"', '\\"') + '"',
        "    },",
        "    {",
        '      "type": "image_url",',
        '      "image_url": {',
        f'        "url": "{BASE_URL}...<base64 trimmed, len={random.randint(100000, 300000)}>"',
        "      }",
        "    }",
        "  ]",
        "}",
        "",
    ]

    # Poll responses
    s += random.randint(1, 5)
    ts1 = rand_ts(h, m, s % 60, 0)
    lines += [
        SEP,
        "响应",
        SEP,
        f"{ts1} 响应状态码: 200",
        f"{ts1} task_id: {rand_tid()}",
    ]
    prog = 0
    for i in range(random.randint(3, 8)):
        s += random.randint(3, 10)
        prog = min(100, prog + random.randint(10, 30))
        ts2 = rand_ts(h, m, s % 60, 0)
        status = "processing" if prog < 100 else "success"
        pstr = f", progress={prog}%" if status == "processing" else ""
        lines.append(f"{ts2} 轮询 #{i+1}: status={status}{pstr}")

    s += random.randint(1, 3)
    ts3 = rand_ts(h, m, s % 60, 0)
    lines += [
        f"{ts3} 视频下载完成, size={random.randint(800, 3000)}KB",
        f"{ts3} === 测试完成 key = {key_name} ===",
        "",
    ]
    return "\n".join(lines)


KEY_NAMES = [
    "SVIP", "poly_spark", "premium_cn", "basic_cn", "test_v2",
    "prod_east", "prod_west", "staging_01", "staging_02", "canary",
    "user_a1", "user_b2", "user_c3", "user_d4", "user_e5",
    "batch_01", "batch_02", "batch_03", "batch_04", "batch_05",
]


def rand_key_val(name: str) -> str:
    prefix = "zyb-" + hashlib.md5(name.encode()).hexdigest()[:20] + "@" + name
    return prefix


SESSIONS_PER_FILE = 5  # multiple test sessions per file → larger, more repetition

print(f"Generating {len(KEY_NAMES)} synthetic log files ({SESSIONS_PER_FILE} sessions each) → {OUT_DIR}")
for i, name in enumerate(KEY_NAMES):
    sessions = []
    for j in range(SESSIONS_PER_FILE):
        h = 16 + (i * SESSIONS_PER_FILE + j) // 10
        m = ((i * SESSIONS_PER_FILE + j) * 3) % 60
        sessions.append(gen_log(name, rand_key_val(name), h, m))
    content = "\n".join(sessions)
    path = os.path.join(OUT_DIR, f"repro_log_{name}.txt")
    with open(path, 'w', encoding='utf-8') as f:
        f.write(content)
    size = len(content.encode('utf-8'))
    print(f"  {path.split(os.sep)[-1]:<30} {size:,} bytes")

print("Done.")
