import Foundation
import MLX
import MLXNN

// =============================================================================
// Projektor PaddleOCR-VL (mlp_AR) — spatial merge 2x2 patchy + MLP do wymiaru
// LM. Port 1:1 z Projector.forward: pre_norm -> rearrange
// "(t h p1 w p2) d -> (t h w) (p1 p2 d)" -> linear_1 -> gelu -> linear_2.
// =============================================================================

public class MultiModalProjector: Module {
    @ModuleInfo(key: "pre_norm") var preNorm: LayerNorm
    @ModuleInfo(key: "linear_1") var linear1: Linear
    @ModuleInfo(key: "linear_2") var linear2: Linear

    let mergeSize: Int
    let visionHidden: Int
    let mergedDim: Int

    public init(config: PaddleOCRVLConfig) {
        self.mergeSize = config.visionConfig.spatialMergeSize
        self.visionHidden = config.visionConfig.hiddenSize
        self.mergedDim = visionHidden * mergeSize * mergeSize

        // pre_norm liczona na wymiarze vision (eps 1e-5 wg referencji).
        self._preNorm.wrappedValue = LayerNorm(dimensions: visionHidden, eps: 1e-5)
        self._linear1.wrappedValue = Linear(mergedDim, mergedDim)
        self._linear2.wrappedValue = Linear(mergedDim, config.textConfig.hiddenSize)
        super.init()
    }

    /// features: [N, visionHidden], N = gridH*gridW. Zwraca [N/(m*m), textHidden].
    public func callAsFunction(_ features: MLXArray, gridH: Int, gridW: Int) -> MLXArray {
        let m = mergeSize
        var x = preNorm(features)  // [N, D]
        // rearrange (h p1 w p2) d -> (h w) (p1 p2 d): t=1
        x = x.reshaped(gridH / m, m, gridW / m, m, visionHidden)  // [h, p1, w, p2, D]
        x = x.transposed(0, 2, 1, 3, 4)                            // [h, w, p1, p2, D]
        x = x.reshaped((gridH / m) * (gridW / m), mergedDim)       // [(h w), (p1 p2 D)]
        x = linear1(x)
        x = geluApproximate(x)
        x = linear2(x)
        return x
    }
}
