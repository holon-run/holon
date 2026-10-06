"""Verified system text-size lifecycle for dedicated iOS UI simulators."""
from contextlib import contextmanager
import subprocess


MAXIMUM_TEXT_SIZE = "accessibility-extra-extra-extra-large"
_TEXT_SIZES = {
    "extra-small", "small", "medium", "large", "extra-large",
    "extra-extra-large", "extra-extra-extra-large", "accessibility-medium",
    "accessibility-large", "accessibility-extra-large",
    "accessibility-extra-extra-large", MAXIMUM_TEXT_SIZE,
}


def _read_text_size(simulator):
    result = subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "content_size"],
        check=True, capture_output=True, text=True,
    )
    category = result.stdout.strip()
    if category not in _TEXT_SIZES:
        raise RuntimeError(f"无法读取可恢复的模拟器系统字号：{category!r}")
    return category


def _set_text_size(simulator, category, phase):
    subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "content_size", category],
        check=True, capture_output=True, text=True,
    )
    actual = _read_text_size(simulator)
    print(f"模拟器 {simulator} 系统字号（{phase}）：{actual}", flush=True)
    if actual != category:
        raise RuntimeError(f"系统字号核验失败：期望 {category}，实际 {actual}")


def initialize_simulator_text_size(simulator):
    """Establish a verified baseline on the harness-owned fresh simulator."""
    _set_text_size(simulator, "large", "专用测试基线初始化")


@contextmanager
def simulator_text_size(simulator, category):
    original = _read_text_size(simulator)
    print(f"模拟器 {simulator} 原系统字号：{original}", flush=True)
    try:
        _set_text_size(simulator, category, "设置")
        yield category
    finally:
        # Also restore if configuration, XCTest or its runtime size change fails.
        _set_text_size(simulator, original, "恢复")
