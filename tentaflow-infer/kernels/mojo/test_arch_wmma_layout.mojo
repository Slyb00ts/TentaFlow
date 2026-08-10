# =============================================================================
# Plik: test_arch_wmma_layout.mojo
# Opis: Sprawdza mapowanie rejestrow akumulatora WMMA dla RDNA3 i RDNA4 bez GPU.
# Przykład: pixi run mojo test_arch_wmma_layout.mojo
# =============================================================================

from src.arch_wmma import wmma_acc_row_rdna3, wmma_acc_row_rdna4


def main() raises:
    for lane in range(32):
        for i in range(8):
            expected_rdna3 = i * 2 + lane // 16
            if wmma_acc_row_rdna3(lane, i) != expected_rdna3:
                raise Error("niepoprawny uklad akumulatora RDNA3")
            expected_rdna4 = 8 * (lane // 16) + i
            if wmma_acc_row_rdna4(lane, i) != expected_rdna4:
                raise Error("niepoprawny uklad akumulatora RDNA4")
    print("WMMA accumulator layouts: PASS")
