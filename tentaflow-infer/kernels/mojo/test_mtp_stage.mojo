# =============================================================================
# Plik: test_mtp_stage.mojo
# Opis: Sprawdza odroczone metadane MTP, granice stron i nienaruszony ogon tabeli.
# Przykład: pixi run mojo test_mtp_stage.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.mtp import mtp_stage_step


def _run_page2(ctx: DeviceContext) raises:
    var position = ctx.enqueue_create_buffer[DType.int32](1)
    var seq_len = ctx.enqueue_create_buffer[DType.int32](1)
    var pages = ctx.enqueue_create_buffer[DType.int32](6)
    with pages.map_to_host() as host:
        for i in range(6):
            host[i] = Int32(91 + i)

    ctx.enqueue_function[mtp_stage_step](position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(), 0, 1, 0, 7, grid_dim=1, block_dim=1)
    ctx.enqueue_function[mtp_stage_step](position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(), 1, 2, -1, -1, grid_dim=1, block_dim=1)
    ctx.enqueue_function[mtp_stage_step](position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(), 2, 3, 1, 11, grid_dim=1, block_dim=1)
    ctx.enqueue_function[mtp_stage_step](position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(), 3, 4, -1, -1, grid_dim=1, block_dim=1)
    ctx.synchronize()

    with position.map_to_host() as pos, seq_len.map_to_host() as length, pages.map_to_host() as table:
        if pos[0] != 3 or length[0] != 4:
            raise Error("niezgodne odroczone metadane dla page_size=2")
        if table[0] != 7 or table[1] != 11:
            raise Error("niezgodne mapowanie granic dla page_size=2")
        for i in range(2, 6):
            if table[i] != Int32(91 + i):
                raise Error("kernel naruszył zatruty ogon dla page_size=2")


def _run_page4(ctx: DeviceContext) raises:
    var position = ctx.enqueue_create_buffer[DType.int32](1)
    var seq_len = ctx.enqueue_create_buffer[DType.int32](1)
    var pages = ctx.enqueue_create_buffer[DType.int32](5)
    with pages.map_to_host() as host:
        for i in range(5):
            host[i] = Int32(71 + i)

    for step in range(4):
        ctx.enqueue_function[mtp_stage_step](
            position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(),
            step, step + 1, 0 if step == 0 else -1, 5 if step == 0 else -1,
            grid_dim=1, block_dim=1,
        )
    ctx.enqueue_function[mtp_stage_step](position.unsafe_ptr(), seq_len.unsafe_ptr(), pages.unsafe_ptr(), 4, 5, 1, 9, grid_dim=1, block_dim=1)
    ctx.synchronize()

    with position.map_to_host() as pos, seq_len.map_to_host() as length, pages.map_to_host() as table:
        if pos[0] != 4 or length[0] != 5:
            raise Error("niezgodne odroczone metadane dla page_size=4")
        if table[0] != 5 or table[1] != 9:
            raise Error("niezgodne mapowanie granic dla page_size=4")
        for i in range(2, 5):
            if table[i] != Int32(71 + i):
                raise Error("kernel naruszył zatruty ogon dla page_size=4")


def main() raises:
    var ctx = DeviceContext()
    _run_page2(ctx)
    _run_page4(ctx)
    print("mtp stage deferred/page boundary/poison tail: PASS")
