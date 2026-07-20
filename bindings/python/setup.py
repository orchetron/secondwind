"""Forces a platform-specific but Python-version-independent wheel (py3-none-<platform>): the bundled
native library is ABI-stable across Python versions (ctypes, no C-API) but specific to OS+arch."""

from setuptools import setup

try:
    from setuptools.command.bdist_wheel import bdist_wheel
except ImportError:  # older setuptools
    from wheel.bdist_wheel import bdist_wheel


class PlatformWheel(bdist_wheel):
    def finalize_options(self):
        super().finalize_options()
        self.root_is_pure = False  # ship a platform wheel, not a pure one

    def get_tag(self):
        _python, _abi, platform = super().get_tag()
        return "py3", "none", platform  # any Python 3, no C-API ABI, this platform only


setup(cmdclass={"bdist_wheel": PlatformWheel})
