#!/usr/bin/env python3
"""Generate original, static Chinese wireframes using Python's standard library."""
from pathlib import Path
from xml.sax.saxutils import escape
import unicodedata
import xml.etree.ElementTree as ET

W, H = 1200, 820
OUT = Path(__file__).resolve().parent
NAV = ["总览", "设备与屏幕", "本机鼠标", "剪贴板与文件", "设置"]
MARKER = "交互线框·示例数据·非运行软件"


class Frame:
    def __init__(self, title, active):
        self.parts = [
            '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="820" viewBox="0 0 1200 820" role="img" aria-labelledby="title desc">',
            f'<title id="title">屏跃 · {escape(title)} · 中文交互线框</title>',
            '<desc id="desc">原创黑白灰低保真设计。所有设备、权限、文件和进度均为示例，不联网、不读取数据、不执行系统动作。</desc>',
            '<g font-family="PingFang SC, Microsoft YaHei, sans-serif">',
        ]
        self.labels = []
        self.rect(0, 0, W, H, fill="#fff", stroke="none", radius=0)
        self.rect(0, 0, 212, H, fill="#f5f5f5", stroke="none", radius=0)
        self.line(212, 0, 212, H, stroke="#d0d0d0")
        self.line(212, 76, W, 76, stroke="#d0d0d0")
        self.text(28, 39, "屏跃", 27, weight=700)
        self.text(28, 65, "鼠标增强与多机协同", 13, color="#666")
        self.text(244, 32, "本机控制 · 连接状态为示例", 15, color="#555")
        self.text(244, 55, "停止与恢复入口始终可见", 13, color="#777")
        self.button(844, 20, 142, 36, "暂停本机手势")
        self.button(1002, 20, 174, 36, "紧急恢复本机控制", primary=True)
        for index, label in enumerate(NAV):
            y = 121 + index * 57
            if index == active:
                self.rect(16, y, 180, 44, fill="#e3e3e3", stroke="#999", radius=5)
            self.rect(29, y + 14, 16, 16, fill="#fff", stroke="#777", radius=2)
            self.text(34, y + 26, str(index + 1), 10, color="#555")
            self.text(58, y + 29, label, 17, weight=600 if index == active else 400)
        self.line(24, 684, 188, 684, stroke="#d4d4d4")
        self.text(28, 714, "本机鼠标：关闭", 14, color="#555")
        self.text(28, 741, "跨屏控制：关闭", 14, color="#555")
        self.text(28, 782, "界面与状态均为示例", 12, color="#777")
        self.line(236, 780, 1180, 780, stroke="#c8c8c8")
        self.text(244, 805, MARKER, 14, weight=600, color="#444")
        self.text(1031, 805, "设计 0.2.0", 13, color="#777")

    def rect(self, x, y, width, height, fill="#fff", stroke="#c4c4c4", radius=7, dash=None):
        assert x >= 0 and y >= 0 and x + width <= W and y + height <= H
        extra = f' stroke-dasharray="{dash}"' if dash else ""
        self.parts.append(f'<rect x="{x}" y="{y}" width="{width}" height="{height}" rx="{radius}" fill="{fill}" stroke="{stroke}"{extra}/>')

    def line(self, x1, y1, x2, y2, stroke="#bbb", width=1, dash=None):
        extra = f' stroke-dasharray="{dash}"' if dash else ""
        self.parts.append(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{stroke}" stroke-width="{width}"{extra}/>')

    def text(self, x, y, value, size=16, weight=400, color="#252525"):
        # An intentionally conservative static estimate, not rendered acceptance.
        units = sum(1 if unicodedata.east_asian_width(c) in "WF" else 0.6 for c in value)
        assert 0 <= x and y - size >= 0 and y <= H, (value, x, y)
        assert x + units * size <= W - 8, (value, x, units * size)
        self.labels.append(value)
        self.parts.append(f'<text x="{x}" y="{y}" font-size="{size}" font-weight="{weight}" fill="{color}">{escape(value)}</text>')

    def lines(self, x, y, values, size=15, gap=24, color="#666"):
        for index, value in enumerate(values):
            self.text(x, y + index * gap, value, size, color=color)

    def button(self, x, y, width, height, label, primary=False, muted=False):
        self.rect(x, y, width, height, fill="#333" if primary else "#f3f3f3" if muted else "#fff", stroke="#444" if primary else "#b6b6b6", radius=5)
        self.text(x + 14, y + height / 2 + 5, label, 15, weight=500, color="#fff" if primary else "#888" if muted else "#333")

    def badge(self, x, y, width, label):
        self.rect(x, y, width, 25, fill="#eee", stroke="#d2d2d2", radius=4)
        self.text(x + 8, y + 18, label, 12, color="#666")

    def off(self, x, y):
        self.rect(x, y, 59, 25, fill="#f1f1f1", stroke="#bcbcbc", radius=12)
        self.rect(x + 5, y + 5, 15, 15, fill="#fff", stroke="#aaa", radius=8)
        self.text(x + 26, y + 17, "关闭", 11, color="#666")

    def title(self, title, subtitle):
        self.text(244, 119, title, 26, weight=650)
        self.text(244, 148, subtitle, 15, color="#666")

    def trace(self, points):
        coords = " ".join(f"{x},{y}" for x, y in points)
        self.parts.append(f'<polyline points="{coords}" fill="none" stroke="#4b4b4b" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>')
        x, y = points[-1]
        px, py = points[-2]
        if x > px:
            self.trace_arrow([(x - 8, y - 6), (x, y), (x - 8, y + 6)])
        elif x < px:
            self.trace_arrow([(x + 8, y - 6), (x, y), (x + 8, y + 6)])
        elif y < py:
            self.trace_arrow([(x - 6, y + 8), (x, y), (x + 6, y + 8)])
        else:
            self.trace_arrow([(x - 6, y - 8), (x, y), (x + 6, y - 8)])

    def trace_arrow(self, points):
        coords = " ".join(f"{x},{y}" for x, y in points)
        self.parts.append(f'<polyline points="{coords}" fill="none" stroke="#4b4b4b" stroke-width="2"/>')

    def save(self, filename):
        self.parts.extend(["</g>", "</svg>"])
        result = "\n".join(self.parts) + "\n"
        root = ET.fromstring(result)
        assert root.attrib["viewBox"] == "0 0 1200 820"
        assert all(label in self.labels for label in NAV)
        assert MARKER in self.labels
        assert "http" not in result.replace('xmlns="http://www.w3.org/2000/svg"', "")
        assert not any(tag in result for tag in ["<script", "<image", "<foreignObject", "href="])
        (OUT / filename).write_text(result, encoding="utf-8")


def overview():
    f = Frame("总览", 0)
    f.title("先从一件事开始", "一台电脑也能用。连接另一台电脑后，再配置协同。")
    f.rect(244, 180, 448, 242)
    f.text(268, 219, "增强本机鼠标", 22, weight=600)
    f.badge(588, 194, 80, "G1 规划")
    f.text(268, 255, "不需要另一台电脑", 18, weight=550)
    f.lines(268, 292, ["录制手势 → 选动作 → 本窗口测试", "保存规则后，再单独决定是否启用。"])
    f.button(268, 356, 208, 39, "去配置本机鼠标", primary=True)
    f.button(492, 356, 176, 39, "先试离线画布")
    f.rect(716, 180, 452, 242)
    f.text(740, 219, "连接另一台电脑", 22, weight=600)
    f.text(740, 255, "把多机协同分步配置", 18, weight=550)
    f.lines(740, 292, ["添加设备，分别核验身份与选择权限。", "连接后摆放屏幕，键鼠控制默认关闭。"])
    f.button(740, 356, 174, 39, "添加设备", primary=True)
    f.button(930, 356, 212, 39, "了解连接步骤")
    f.rect(244, 448, 924, 112, fill="#fafafa")
    for x, title, state, detail in [
        (268, "本机鼠标", "关闭", "可先离线配置"),
        (572, "跨屏键鼠", "关闭", "连接后单独授权"),
        (876, "文本与文件", "关闭", "权限互不代替"),
    ]:
        f.text(x, 480, title, 16, weight=550)
        f.text(x, 509, state, 18, weight=600)
        f.text(x + 62, 508, detail, 14, color="#666")
    f.line(550, 466, 550, 541, stroke="#ddd")
    f.line(854, 466, 854, 541, stroke="#ddd")
    f.text(244, 600, "下一步", 19, weight=600)
    f.rect(244, 620, 448, 126)
    f.text(268, 652, "完成第一条鼠标规则", 18, weight=550)
    f.text(268, 680, "先录制一个轨迹，再做本窗口测试。", 15, color="#666")
    f.button(268, 700, 136, 31, "继续配置")
    f.rect(716, 620, 452, 126)
    f.text(740, 652, "添加第一台电脑", 18, weight=550)
    f.text(740, 680, "先建立信任，再考虑跨屏控制。", 15, color="#666")
    f.button(740, 700, 136, 31, "开始添加")
    f.save("overview.svg")


def devices():
    f = Frame("设备与屏幕", 1)
    f.title("设备与屏幕", "先连接，再摆放。屏幕位置不会自动开启键鼠控制。")
    f.rect(244, 178, 580, 324)
    f.text(268, 211, "屏幕布局", 20, weight=600)
    f.badge(672, 192, 128, "拖动布局 · 规划")
    f.text(268, 243, "拖动屏幕，设置相对位置（布局示例）", 15, color="#666")
    f.rect(284, 279, 208, 125, fill="#eee", stroke="#999", radius=5)
    f.rect(550, 279, 230, 125, fill="#f8f8f8", stroke="#999", radius=5, dash="5 4")
    f.text(340, 328, "本机 Mac", 19, weight=550)
    f.text(329, 360, "当前控制在本机", 14, color="#666")
    f.text(585, 328, "Windows 电脑", 18, weight=550)
    f.text(604, 360, "示例 · 未连接", 14, color="#666")
    f.line(499, 342, 542, 342, stroke="#999", width=2, dash="4 4")
    f.line(388, 405, 388, 418, stroke="#999", width=2)
    f.line(352, 419, 424, 419, stroke="#999", width=2)
    f.line(665, 405, 665, 418, stroke="#999", width=2)
    f.line(626, 419, 704, 419, stroke="#999", width=2)
    f.text(268, 468, "相对位置仅为示例，连接不会自动开启跨屏。", 14, color="#666")
    f.rect(844, 178, 324, 324)
    f.text(868, 211, "Windows 电脑", 20, weight=600)
    f.text(868, 241, "已登记 · 尚未连接（示例）", 14, color="#666")
    f.text(868, 280, "本机允许对方控制键鼠", 14)
    f.off(1084, 262)
    f.text(868, 307, "对方允许本机：由对端选择", 13, color="#777")
    f.text(868, 345, "发送本机文本", 14)
    f.off(1084, 328)
    f.text(868, 387, "接收对方文件", 14)
    f.off(1084, 370)
    f.button(868, 429, 276, 39, "查看设备权限与连接")
    f.rect(244, 526, 924, 220)
    f.text(268, 562, "添加设备", 20, weight=600)
    f.button(990, 543, 154, 36, "打开添加向导", primary=True)
    for index, (x, label) in enumerate([(284, "查找地址"), (459, "核验身份"), (634, "登记设备"), (809, "选择权限"), (984, "建立连接")]):
        f.rect(x, 601, 30, 30, fill="#eee", stroke="#aaa", radius=15)
        f.text(x + 10, 623, str(index + 1), 16)
        f.text(x - 15, 658, label, 16)
        if index < 4:
            f.line(x + 45, 616, x + 140, 616, stroke="#ccc")
    f.text(268, 700, "发现结果未被信任；登记与连接均不代表获得控制权限。", 15, color="#666")
    f.text(268, 727, "身份核验使用公开证书，配对码与扫码属于后续规划。", 13, color="#777")
    f.save("devices.svg")


def mouse():
    f = Frame("本机鼠标", 2)
    f.title("本机鼠标", "无需另一台电脑。先录制、测试并保存，再单独考虑启用。")
    f.rect(244, 178, 924, 98, fill="#f7f7f7")
    f.text(268, 214, "全局手势", 19, weight=600)
    f.off(374, 194)
    f.badge(450, 195, 80, "G1 规划")
    f.text(268, 250, "全局启用仍在规划；窗口内测试不执行动作。", 15, color="#666")
    f.button(790, 205, 160, 39, "仅本窗口测试")
    f.button(966, 205, 178, 39, "查看启用条件")
    f.rect(244, 296, 924, 288)
    f.text(268, 330, "我的规则", 20, weight=600)
    f.button(1028, 309, 116, 34, "新建规则", primary=True)
    f.rect(260, 352, 892, 38, fill="#f1f1f1", stroke="none", radius=0)
    for x, label in [(280, "手势轨迹"), (456, "应用范围"), (742, "动作"), (1032, "状态")]:
        f.text(x, 378, label, 15, weight=550, color="#555")
    for y in [452, 514]:
        f.line(260, y, 1152, y, stroke="#ddd")
    f.trace([(288, 422), (345, 422)])
    f.trace([(290, 468), (290, 492), (345, 492)])
    f.trace([(317, 564), (317, 530)])
    for y, app, action in [(428, "所有应用", "最小化窗口"), (491, "示例浏览器", "关闭窗口 · 需确认"), (553, "示例编辑器", "最大化窗口")]:
        f.text(456, y, app, 16)
        f.text(742, y, action, 16)
        f.text(1032, y, "草稿", 14, color="#777")
    f.rect(244, 608, 566, 138)
    f.text(268, 640, "本窗口测试", 19, weight=600)
    f.rect(268, 655, 226, 66, fill="#fafafa", stroke="#aaa", radius=4, dash="4 4")
    f.trace([(288, 690), (324, 690), (324, 678), (356, 678)])
    f.text(522, 675, "按住右键绘制轨迹", 15)
    f.text(522, 704, "只显示识别，不执行动作", 14, color="#666")
    f.rect(830, 608, 338, 138, fill="#f7f7f7")
    f.text(850, 640, "保存规则 ≠ 启用手势", 18, weight=600)
    f.lines(850, 667, ["保存只写入规则；全局捕获需", "单独同意，并满足系统条件。"], size=14, gap=22)
    f.button(850, 704, 130, 29, "保存草稿")
    f.text(244, 767, "应用规则优先，排除始终优先；示例名称不代替精确应用标识。", 13, color="#777")
    f.save("mouse.svg")


def collaboration():
    f = Frame("剪贴板与文件", 3)
    f.title("剪贴板与文件", "选择对象与方向，文本和文件分别授权。")
    f.rect(244, 178, 924, 200)
    f.text(268, 211, "文本同步", 20, weight=600)
    f.badge(382, 191, 118, "数据协作 · 规划")
    f.button(1006, 191, 138, 35, "手动发送文本", muted=True)
    f.text(268, 250, "目标设备", 15, color="#666")
    f.rect(376, 229, 324, 32, fill="#fafafa", stroke="#bbb", radius=4)
    f.text(390, 251, "Windows 电脑（示例）", 15)
    f.text(268, 293, "本机 → Windows 电脑", 16)
    f.text(776, 292, "允许发送本机文本", 14)
    f.off(1084, 273)
    f.text(268, 333, "Windows 电脑 → 本机", 16)
    f.text(776, 332, "允许接收对方文本", 14)
    f.off(1084, 313)
    f.text(268, 362, "两个方向分别同意；对方的发送许可仍由对方设置。", 14, color="#777")
    f.rect(244, 400, 450, 214)
    f.text(268, 434, "待接收", 20, weight=600)
    f.badge(542, 414, 128, "需本次确认")
    f.text(268, 473, "设计草稿.pdf", 18, weight=550)
    f.text(268, 503, "来自 Windows 电脑 · 2.4 MB（示例）", 14, color="#666")
    f.lines(268, 533, ["保存位置由你选择，重名不会覆盖原文件。", "接收默认关闭；确认前不保存文件。"], size=14, gap=22)
    f.button(268, 571, 242, 30, "选择位置并接收一次")
    f.button(526, 571, 144, 30, "拒绝")
    f.rect(716, 400, 452, 214)
    f.text(740, 434, "传输进度（示例）", 20, weight=600)
    f.text(740, 473, "项目资料.zip", 18, weight=550)
    f.text(740, 503, "已暂停 · 已接收 42 MB / 100 MB", 14, color="#666")
    f.rect(740, 525, 404, 13, fill="#eee", stroke="#ccc", radius=4)
    f.rect(740, 525, 170, 13, fill="#888", stroke="none", radius=4)
    f.button(740, 571, 176, 30, "继续（示例）")
    f.button(932, 571, 212, 30, "取消本次传输")
    f.rect(244, 638, 924, 108, fill="#f7f7f7")
    f.text(268, 674, "接收前看清对象、方向与保存位置。", 18, weight=550)
    f.text(268, 706, "“传输完成”仅在校验与保存成功后显示；跨屏拖拽是另一项后续能力。", 15, color="#666")
    f.save("collaboration.svg")


def settings():
    f = Frame("设置", 4)
    f.title("设置", "查看系统条件，保留原配置，并明确恢复方式。")
    f.rect(244, 178, 450, 264)
    f.text(268, 214, "系统权限", 20, weight=600)
    for y, name in [(256, "辅助功能"), (294, "输入监控"), (332, "当前能力")]:
        f.text(268, y, name, 16)
        f.text(570, y, "未启用" if name == "当前能力" else "待检查", 15, color="#777")
    f.button(268, 359, 238, 37, "查看系统设置指引")
    f.text(268, 422, "只提供指引，不会替你修改系统权限。", 14, color="#777")
    f.rect(716, 178, 452, 264)
    f.text(740, 214, "外观与窗口", 20, weight=600)
    f.text(740, 253, "外观", 16)
    for x, width, label, selected in [(740, 142, "跟随系统", True), (882, 126, "浅色", False), (1008, 136, "深色", False)]:
        f.rect(x, 270, width, 35, fill="#ddd" if selected else "#fff", stroke="#bbb", radius=3)
        f.text(x + 19, 293, label, 15)
    f.rect(740, 327, 16, 16, fill="#eee", stroke="#888", radius=2)
    f.line(743, 335, 747, 339, stroke="#555", width=2)
    f.line(747, 339, 753, 330, stroke="#555", width=2)
    f.text(770, 341, "显示连接摘要", 15)
    f.text(740, 377, "关闭主窗口：退出应用", 16)
    f.text(740, 421, "托盘尚未开放时，不暗示继续后台运行。", 14, color="#777")
    f.rect(244, 466, 450, 280)
    f.text(268, 501, "配置与恢复", 20, weight=600)
    f.rect(268, 520, 402, 44, fill="#eee", stroke="#bbb", radius=4)
    f.text(282, 548, "示例：配置需要恢复，当前只读", 16, weight=550)
    f.lines(268, 597, ["原文件与身份会保留。", "可先查看备份，再决定恢复。"], size=15, gap=25)
    f.button(268, 653, 178, 36, "查看可用备份")
    f.button(460, 653, 210, 36, "导出诊断（示例）")
    f.text(268, 721, "重置前先展示保留范围与备份说明。", 14, color="#777")
    f.rect(716, 466, 452, 280)
    f.text(740, 501, "停止与恢复", 20, weight=600)
    f.lines(740, 540, ["暂停：立即停止接收新的手势动作。", "紧急恢复：请求停止控制，恢复本机输入。"], size=15, gap=28)
    f.rect(740, 590, 404, 63, fill="#eee", stroke="#c1c1c1", radius=4)
    f.lines(754, 615, ["若后端未确认清理完成：", "显示“恢复尚未完成”，保持停止状态。"], size=14, gap=22, color="#555")
    f.button(740, 682, 240, 37, "查看异常处理说明")
    f.save("settings.svg")


if __name__ == "__main__":
    for build in [overview, devices, mouse, collaboration, settings]:
        build()
    print("Generated 5 static Chinese SVG wireframes; XML and bounds checks passed.")
