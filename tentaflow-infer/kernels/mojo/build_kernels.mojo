# =============================================================================
# Plik: build_kernels.mojo
# Opis: Uruchamia izolowana kompilacje katalogu kerneli Mojo i buduje manifest.
# Przykład: pixi run mojo build_kernels.mojo
# =============================================================================

from std.python import Python


def main() raises:
    Python.add_to_path("scripts")
    builder = Python.import_module("build_kernel_catalog")
    builder.main()
