// Generated from ONNX "plate_ocr.onnx" by burn-onnx.
// MANUAL FIX (re-apply after any regeneration): the 6 `Interpolate2d` ops were
// emitted with float `with_scale_factor`, which rounds to the wrong spatial size
// (e.g. H=4 instead of 6) and panics the downstream window `reshape`. They are
// pinned to exact `with_output_size` from ONNX shape inference (NCHW H,W):
// Resize 1..6 = [10,18],[9,18],[6,10],[5,9],[4,6],[3,5]. Model is fixed-input 70x140.
use burn::nn::conv::Conv2d;
use burn::nn::conv::Conv2dConfig;
use burn::nn::Linear;
use burn::nn::LinearConfig;
use burn::nn::PaddingConfig2d;
use burn::prelude::*;
use burn::tensor::Bytes;
use burn_store::BurnpackStore;
use burn_store::ModuleSnapshot;

#[derive(Module, Debug)]
pub struct Submodule1<B: Backend> {
    constant171: burn::module::Param<Tensor<B, 1>>,
    conv2d1: Conv2d<B>,
    conv2d2: Conv2d<B>,
    conv2d3: Conv2d<B>,
    constant170: burn::module::Param<Tensor<B, 4>>,
    constant190: burn::module::Param<Tensor<B, 4>>,
    conv2d4: Conv2d<B>,
    conv2d5: Conv2d<B>,
    conv2d6: Conv2d<B>,
    constant165: burn::module::Param<Tensor<B, 4>>,
    constant199: burn::module::Param<Tensor<B, 4>>,
    conv2d7: Conv2d<B>,
    conv2d8: Conv2d<B>,
    conv2d9: Conv2d<B>,
    constant160: burn::module::Param<Tensor<B, 4>>,
    constant193: burn::module::Param<Tensor<B, 4>>,
    conv2d10: Conv2d<B>,
    conv2d11: Conv2d<B>,
    conv2d12: Conv2d<B>,
    constant155: burn::module::Param<Tensor<B, 4>>,
    constant198: burn::module::Param<Tensor<B, 4>>,
    conv2d13: Conv2d<B>,
    conv2d14: Conv2d<B>,
    constant149: burn::module::Param<Tensor<B, 4>>,
    constant197: burn::module::Param<Tensor<B, 4>>,
    conv2d15: Conv2d<B>,
    resize1: burn::nn::interpolate::Interpolate2d,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule1<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant171: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 1>::from_data(
                    burn::tensor::TensorData::from([0.003921568859368563f64]),
                    (device, burn::tensor::DType::F32),
                )
            },
            device.clone(),
            false,
            [1].into(),
        );
        let conv2d1 = Conv2dConfig::new([1, 16], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d2 = Conv2dConfig::new([16, 32], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d3 = Conv2dConfig::new([32, 32], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(32)
            .with_bias(false)
            .init(device);
        let constant170: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 32, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 32, 1, 1].into(),
        );
        let constant190: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 32, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 32, 1, 1].into(),
        );
        let conv2d4 = Conv2dConfig::new([32, 32], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d5 = Conv2dConfig::new([32, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d6 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(64)
            .with_bias(false)
            .init(device);
        let constant165: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 64, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 64, 1, 1].into(),
        );
        let constant199: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 64, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 64, 1, 1].into(),
        );
        let conv2d7 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d8 = Conv2dConfig::new([64, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d9 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(128)
            .with_bias(false)
            .init(device);
        let constant160: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let constant193: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let conv2d10 = Conv2dConfig::new([128, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d11 = Conv2dConfig::new([64, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d12 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(128)
            .with_bias(false)
            .init(device);
        let constant155: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let constant198: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let conv2d13 = Conv2dConfig::new([128, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d14 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(128)
            .with_bias(false)
            .init(device);
        let constant149: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let constant197: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 128, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 128, 1, 1].into(),
        );
        let conv2d15 = Conv2dConfig::new([128, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let resize1 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([10, 18]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        Self {
            constant171,
            conv2d1,
            conv2d2,
            conv2d3,
            constant170,
            constant190,
            conv2d4,
            conv2d5,
            conv2d6,
            constant165,
            constant199,
            conv2d7,
            conv2d8,
            conv2d9,
            constant160,
            constant193,
            conv2d10,
            conv2d11,
            conv2d12,
            constant155,
            constant198,
            conv2d13,
            conv2d14,
            constant149,
            constant197,
            conv2d15,
            resize1,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, input: Tensor<B, 4, Int>) -> Tensor<B, 6> {
        let cast1_out1 = input.float().cast(burn::tensor::DType::F32);
        let constant171_out1 = self.constant171.val();
        let mul1_out1 =
            cast1_out1.mul((constant171_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let reshape1_out1 = mul1_out1.reshape([-1, 1, 70, 140]);
        let conv2d1_out1 = self.conv2d1.forward(reshape1_out1);
        let sigmoid1_out1 = burn::tensor::activation::sigmoid(conv2d1_out1.clone());
        let mul2_out1 = conv2d1_out1.mul(sigmoid1_out1);
        let conv2d2_out1 = self.conv2d2.forward(mul2_out1);
        let sigmoid2_out1 = burn::tensor::activation::sigmoid(conv2d2_out1.clone());
        let mul3_out1 = conv2d2_out1.mul(sigmoid2_out1);
        let conv2d3_out1 = self.conv2d3.forward(mul3_out1);
        let constant170_out1 = self.constant170.val();
        let mul4_out1 = conv2d3_out1.mul(constant170_out1);
        let constant190_out1 = self.constant190.val();
        let add1_out1 = mul4_out1.add(constant190_out1);
        let sigmoid3_out1 = burn::tensor::activation::sigmoid(add1_out1.clone());
        let mul5_out1 = add1_out1.mul(sigmoid3_out1);
        let conv2d4_out1 = self.conv2d4.forward(mul5_out1);
        let conv2d5_out1 = self.conv2d5.forward(conv2d4_out1);
        let sigmoid4_out1 = burn::tensor::activation::sigmoid(conv2d5_out1.clone());
        let mul6_out1 = conv2d5_out1.mul(sigmoid4_out1);
        let conv2d6_out1 = self.conv2d6.forward(mul6_out1);
        let constant165_out1 = self.constant165.val();
        let mul7_out1 = conv2d6_out1.mul(constant165_out1);
        let constant199_out1 = self.constant199.val();
        let add2_out1 = mul7_out1.add(constant199_out1);
        let sigmoid5_out1 = burn::tensor::activation::sigmoid(add2_out1.clone());
        let mul8_out1 = add2_out1.mul(sigmoid5_out1);
        let conv2d7_out1 = self.conv2d7.forward(mul8_out1);
        let conv2d8_out1 = self.conv2d8.forward(conv2d7_out1.clone());
        let sigmoid6_out1 = burn::tensor::activation::sigmoid(conv2d8_out1.clone());
        let mul9_out1 = conv2d8_out1.mul(sigmoid6_out1);
        let conv2d9_out1 = self.conv2d9.forward(mul9_out1);
        let constant160_out1 = self.constant160.val();
        let mul10_out1 = conv2d9_out1.mul(constant160_out1);
        let constant193_out1 = self.constant193.val();
        let add3_out1 = mul10_out1.add(constant193_out1);
        let sigmoid7_out1 = burn::tensor::activation::sigmoid(add3_out1.clone());
        let mul11_out1 = add3_out1.mul(sigmoid7_out1);
        let conv2d10_out1 = self.conv2d10.forward(mul11_out1);
        let add4_out1 = conv2d10_out1.add(conv2d7_out1);
        let conv2d11_out1 = self.conv2d11.forward(add4_out1);
        let sigmoid8_out1 = burn::tensor::activation::sigmoid(conv2d11_out1.clone());
        let mul12_out1 = conv2d11_out1.mul(sigmoid8_out1);
        let conv2d12_out1 = self.conv2d12.forward(mul12_out1);
        let constant155_out1 = self.constant155.val();
        let mul13_out1 = conv2d12_out1.mul(constant155_out1);
        let constant198_out1 = self.constant198.val();
        let add5_out1 = mul13_out1.add(constant198_out1);
        let sigmoid9_out1 = burn::tensor::activation::sigmoid(add5_out1.clone());
        let mul14_out1 = add5_out1.mul(sigmoid9_out1);
        let conv2d13_out1 = self.conv2d13.forward(mul14_out1);
        let conv2d14_out1 = self.conv2d14.forward(conv2d13_out1);
        let constant149_out1 = self.constant149.val();
        let mul15_out1 = conv2d14_out1.mul(constant149_out1);
        let constant197_out1 = self.constant197.val();
        let add6_out1 = mul15_out1.add(constant197_out1);
        let sigmoid10_out1 = burn::tensor::activation::sigmoid(add6_out1.clone());
        let mul16_out1 = add6_out1.mul(sigmoid10_out1);
        let conv2d15_out1 = self.conv2d15.forward(mul16_out1);
        let resize1_out1 = self.resize1.forward(conv2d15_out1);
        let transpose1_out1 = resize1_out1.permute([0, 2, 3, 1]);
        let reshape2_out1 = transpose1_out1.reshape([-1, 5, 2, 9, 2, 64]);
        let transpose2_out1 = reshape2_out1.permute([0, 2, 4, 1, 3, 5]);
        transpose2_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule2<B: Backend> {
    constant14: burn::module::Param<Tensor<B, 1>>,
    constant136: burn::module::Param<Tensor<B, 5>>,
    constant135: burn::module::Param<Tensor<B, 5>>,
    conv2d16: Conv2d<B>,
    conv2d17: Conv2d<B>,
    constant134: burn::module::Param<Tensor<B, 5>>,
    constant133: burn::module::Param<Tensor<B, 5>>,
    conv2d18: Conv2d<B>,
    conv2d19: Conv2d<B>,
    constant124: burn::module::Param<Tensor<B, 5>>,
    constant123: burn::module::Param<Tensor<B, 5>>,
    conv2d20: Conv2d<B>,
    conv2d21: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule2<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant14: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 1>::from_data(
                    burn::tensor::TensorData::from([0.000009999999747378752f64]),
                    (device, burn::tensor::DType::F32),
                )
            },
            device.clone(),
            false,
            [1].into(),
        );
        let constant136: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let constant135: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let conv2d16 = Conv2dConfig::new([64, 129], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d17 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant134: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let constant133: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let conv2d18 = Conv2dConfig::new([64, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d19 = Conv2dConfig::new([128, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant124: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let constant123: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let conv2d20 = Conv2dConfig::new([64, 129], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d21 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            constant14,
            constant136,
            constant135,
            conv2d16,
            conv2d17,
            constant134,
            constant133,
            conv2d18,
            conv2d19,
            constant124,
            constant123,
            conv2d20,
            conv2d21,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, transpose2_out1: Tensor<B, 6>) -> (Tensor<B, 4>, Tensor<B, 1>) {
        let reshape3_out1 = transpose2_out1.reshape([-1, 4, 45, 64]);
        let reshape4_out1 = reshape3_out1.clone().reshape([-1, 4, 45, 1, 64]);
        let reducemean1_out1 = {
            reshape4_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg1_out1 = reducemean1_out1.clone().neg();
        let sub1_out1 = reshape4_out1.clone().sub(reducemean1_out1);
        let mul17_out1 = sub1_out1.clone().mul(sub1_out1);
        let reducemean2_out1 = {
            mul17_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let constant14_out1 = self.constant14.val();
        let add7_out1 = reducemean2_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt1_out1 = add7_out1.sqrt();
        let reciprocal1_out1 = sqrt1_out1.recip();
        let constant136_out1 = self.constant136.val();
        let mul18_out1 = reciprocal1_out1.mul(constant136_out1);
        let mul19_out1 = reshape4_out1.mul(mul18_out1.clone());
        let mul20_out1 = neg1_out1.mul(mul18_out1);
        let constant135_out1 = self.constant135.val();
        let add8_out1 = mul20_out1.add(constant135_out1);
        let add9_out1 = mul19_out1.add(add8_out1);
        let reshape5_out1 = add9_out1.reshape([-1, 4, 45, 64]);
        let transpose3_out1 = reshape5_out1.permute([0, 3, 1, 2]);
        let conv2d16_out1 = self.conv2d16.forward(transpose3_out1);
        let transpose4_out1 = conv2d16_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose4_out1.split_with_sizes([1, 64, 64].into(), 3);
        let [split1_out1, split1_out2, split1_out3] = split_tensors.try_into().unwrap();
        let relu1_out1 = burn::tensor::activation::relu(split1_out3);
        let softmax1_out1 = burn::tensor::activation::softmax(split1_out1, 2);
        let mul21_out1 = split1_out2.mul(softmax1_out1);
        let reducesum1_out1 = { mul21_out1.sum_dim(2usize) };
        let mul22_out1 = relu1_out1.mul(reducesum1_out1);
        let transpose5_out1 = mul22_out1.permute([0, 3, 1, 2]);
        let conv2d17_out1 = self.conv2d17.forward(transpose5_out1);
        let transpose6_out1 = reshape3_out1.permute([0, 3, 1, 2]);
        let add10_out1 = transpose6_out1.add(conv2d17_out1);
        let transpose7_out1 = add10_out1.clone().permute([0, 2, 3, 1]);
        let reshape6_out1 = transpose7_out1.reshape([-1, 4, 45, 1, 64]);
        let reducemean3_out1 = {
            reshape6_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg2_out1 = reducemean3_out1.clone().neg();
        let sub2_out1 = reshape6_out1.clone().sub(reducemean3_out1);
        let mul23_out1 = sub2_out1.clone().mul(sub2_out1);
        let reducemean4_out1 = {
            mul23_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add11_out1 = reducemean4_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt2_out1 = add11_out1.sqrt();
        let reciprocal2_out1 = sqrt2_out1.recip();
        let constant134_out1 = self.constant134.val();
        let mul24_out1 = reciprocal2_out1.mul(constant134_out1);
        let mul25_out1 = reshape6_out1.mul(mul24_out1.clone());
        let mul26_out1 = neg2_out1.mul(mul24_out1);
        let constant133_out1 = self.constant133.val();
        let add12_out1 = mul26_out1.add(constant133_out1);
        let add13_out1 = mul25_out1.add(add12_out1);
        let reshape7_out1 = add13_out1.reshape([-1, 4, 45, 64]);
        let transpose8_out1 = reshape7_out1.permute([0, 3, 1, 2]);
        let conv2d18_out1 = self.conv2d18.forward(transpose8_out1);
        let sigmoid11_out1 = burn::tensor::activation::sigmoid(conv2d18_out1.clone());
        let mul27_out1 = conv2d18_out1.mul(sigmoid11_out1);
        let conv2d19_out1 = self.conv2d19.forward(mul27_out1);
        let add14_out1 = add10_out1.add(conv2d19_out1);
        let transpose9_out1 = add14_out1.clone().permute([0, 2, 3, 1]);
        let reshape8_out1 = transpose9_out1.reshape([-1, 4, 45, 1, 64]);
        let reducemean5_out1 = {
            reshape8_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg3_out1 = reducemean5_out1.clone().neg();
        let sub3_out1 = reshape8_out1.clone().sub(reducemean5_out1);
        let mul28_out1 = sub3_out1.clone().mul(sub3_out1);
        let reducemean6_out1 = {
            mul28_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add15_out1 = reducemean6_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt3_out1 = add15_out1.sqrt();
        let reciprocal3_out1 = sqrt3_out1.recip();
        let constant124_out1 = self.constant124.val();
        let mul29_out1 = reciprocal3_out1.mul(constant124_out1);
        let mul30_out1 = reshape8_out1.mul(mul29_out1.clone());
        let mul31_out1 = neg3_out1.mul(mul29_out1);
        let constant123_out1 = self.constant123.val();
        let add16_out1 = mul31_out1.add(constant123_out1);
        let add17_out1 = mul30_out1.add(add16_out1);
        let reshape9_out1 = add17_out1.reshape([-1, 4, 45, 64]);
        let transpose10_out1 = reshape9_out1.permute([0, 3, 1, 2]);
        let conv2d20_out1 = self.conv2d20.forward(transpose10_out1);
        let transpose11_out1 = conv2d20_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose11_out1.split_with_sizes([1, 64, 64].into(), 3);
        let [split2_out1, split2_out2, split2_out3] = split_tensors.try_into().unwrap();
        let relu2_out1 = burn::tensor::activation::relu(split2_out3);
        let softmax2_out1 = burn::tensor::activation::softmax(split2_out1, 2);
        let mul32_out1 = split2_out2.mul(softmax2_out1);
        let reducesum2_out1 = { mul32_out1.sum_dim(2usize) };
        let mul33_out1 = relu2_out1.mul(reducesum2_out1);
        let transpose12_out1 = mul33_out1.permute([0, 3, 1, 2]);
        let conv2d21_out1 = self.conv2d21.forward(transpose12_out1);
        let add18_out1 = add14_out1.add(conv2d21_out1);
        (add18_out1, constant14_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule3<B: Backend> {
    constant122: burn::module::Param<Tensor<B, 5>>,
    constant121: burn::module::Param<Tensor<B, 5>>,
    conv2d22: Conv2d<B>,
    conv2d23: Conv2d<B>,
    constant146: burn::module::Param<Tensor<B, 5>>,
    constant145: burn::module::Param<Tensor<B, 5>>,
    resize2: burn::nn::interpolate::Interpolate2d,
    conv2d24: Conv2d<B>,
    conv2d25: Conv2d<B>,
    conv2d26: Conv2d<B>,
    constant120: burn::module::Param<Tensor<B, 4>>,
    constant195: burn::module::Param<Tensor<B, 4>>,
    conv2d27: Conv2d<B>,
    conv2d28: Conv2d<B>,
    constant114: burn::module::Param<Tensor<B, 4>>,
    constant192: burn::module::Param<Tensor<B, 4>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule3<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant122: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let constant121: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let conv2d22 = Conv2dConfig::new([64, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d23 = Conv2dConfig::new([128, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant146: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let constant145: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 64], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 64].into(),
        );
        let resize2 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([9, 18]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        let conv2d24 = Conv2dConfig::new([64, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d25 = Conv2dConfig::new([128, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d26 = Conv2dConfig::new([256, 256], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(256)
            .with_bias(false)
            .init(device);
        let constant120: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 256, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 256, 1, 1].into(),
        );
        let constant195: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 256, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 256, 1, 1].into(),
        );
        let conv2d27 = Conv2dConfig::new([256, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d28 = Conv2dConfig::new([192, 192], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(192)
            .with_bias(false)
            .init(device);
        let constant114: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 192, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 192, 1, 1].into(),
        );
        let constant192: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 192, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 192, 1, 1].into(),
        );
        Self {
            constant122,
            constant121,
            conv2d22,
            conv2d23,
            constant146,
            constant145,
            resize2,
            conv2d24,
            conv2d25,
            conv2d26,
            constant120,
            constant195,
            conv2d27,
            conv2d28,
            constant114,
            constant192,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add18_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 4> {
        let transpose13_out1 = add18_out1.clone().permute([0, 2, 3, 1]);
        let reshape10_out1 = transpose13_out1.reshape([-1, 4, 45, 1, 64]);
        let reducemean7_out1 = {
            reshape10_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg4_out1 = reducemean7_out1.clone().neg();
        let sub4_out1 = reshape10_out1.clone().sub(reducemean7_out1);
        let mul34_out1 = sub4_out1.clone().mul(sub4_out1);
        let reducemean8_out1 = {
            mul34_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add19_out1 = reducemean8_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt4_out1 = add19_out1.sqrt();
        let reciprocal4_out1 = sqrt4_out1.recip();
        let constant122_out1 = self.constant122.val();
        let mul35_out1 = reciprocal4_out1.mul(constant122_out1);
        let mul36_out1 = reshape10_out1.mul(mul35_out1.clone());
        let mul37_out1 = neg4_out1.mul(mul35_out1);
        let constant121_out1 = self.constant121.val();
        let add20_out1 = mul37_out1.add(constant121_out1);
        let add21_out1 = mul36_out1.add(add20_out1);
        let reshape11_out1 = add21_out1.reshape([-1, 4, 45, 64]);
        let transpose14_out1 = reshape11_out1.permute([0, 3, 1, 2]);
        let conv2d22_out1 = self.conv2d22.forward(transpose14_out1);
        let sigmoid12_out1 = burn::tensor::activation::sigmoid(conv2d22_out1.clone());
        let mul38_out1 = conv2d22_out1.mul(sigmoid12_out1);
        let conv2d23_out1 = self.conv2d23.forward(mul38_out1);
        let add22_out1 = add18_out1.add(conv2d23_out1);
        let transpose15_out1 = add22_out1.permute([0, 2, 3, 1]);
        let reshape12_out1 = transpose15_out1.reshape([-1, 4, 45, 1, 64]);
        let reducemean9_out1 = {
            reshape12_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg5_out1 = reducemean9_out1.clone().neg();
        let sub5_out1 = reshape12_out1.clone().sub(reducemean9_out1);
        let mul39_out1 = sub5_out1.clone().mul(sub5_out1);
        let reducemean10_out1 = {
            mul39_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add23_out1 = reducemean10_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt5_out1 = add23_out1.sqrt();
        let reciprocal5_out1 = sqrt5_out1.recip();
        let constant146_out1 = self.constant146.val();
        let mul40_out1 = reciprocal5_out1.mul(constant146_out1);
        let mul41_out1 = reshape12_out1.mul(mul40_out1.clone());
        let mul42_out1 = neg5_out1.mul(mul40_out1);
        let constant145_out1 = self.constant145.val();
        let add24_out1 = mul42_out1.add(constant145_out1);
        let add25_out1 = mul41_out1.add(add24_out1);
        let reshape13_out1 = add25_out1.reshape([-1, 2, 2, 5, 9, 64]);
        let transpose16_out1 = reshape13_out1.permute([0, 3, 1, 4, 2, 5]);
        let reshape14_out1 = transpose16_out1.reshape([-1, 10, 18, 64]);
        let transpose17_out1 = reshape14_out1.permute([0, 3, 1, 2]);
        let resize2_out1 = self.resize2.forward(transpose17_out1);
        let conv2d24_out1 = self.conv2d24.forward(resize2_out1);
        let conv2d25_out1 = self.conv2d25.forward(conv2d24_out1);
        let sigmoid13_out1 = burn::tensor::activation::sigmoid(conv2d25_out1.clone());
        let mul43_out1 = conv2d25_out1.mul(sigmoid13_out1);
        let conv2d26_out1 = self.conv2d26.forward(mul43_out1);
        let constant120_out1 = self.constant120.val();
        let mul44_out1 = conv2d26_out1.mul(constant120_out1);
        let constant195_out1 = self.constant195.val();
        let add26_out1 = mul44_out1.add(constant195_out1);
        let sigmoid14_out1 = burn::tensor::activation::sigmoid(add26_out1.clone());
        let mul45_out1 = add26_out1.mul(sigmoid14_out1);
        let conv2d27_out1 = self.conv2d27.forward(mul45_out1);
        let conv2d28_out1 = self.conv2d28.forward(conv2d27_out1);
        let constant114_out1 = self.constant114.val();
        let mul46_out1 = conv2d28_out1.mul(constant114_out1);
        let constant192_out1 = self.constant192.val();
        let add27_out1 = mul46_out1.add(constant192_out1);
        let sigmoid15_out1 = burn::tensor::activation::sigmoid(add27_out1.clone());
        let mul47_out1 = add27_out1.mul(sigmoid15_out1);
        mul47_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule4<B: Backend> {
    conv2d29: Conv2d<B>,
    resize3: burn::nn::interpolate::Interpolate2d,
    constant101: burn::module::Param<Tensor<B, 5>>,
    constant100: burn::module::Param<Tensor<B, 5>>,
    conv2d30: Conv2d<B>,
    conv2d31: Conv2d<B>,
    constant99: burn::module::Param<Tensor<B, 5>>,
    constant98: burn::module::Param<Tensor<B, 5>>,
    conv2d32: Conv2d<B>,
    conv2d33: Conv2d<B>,
    constant89: burn::module::Param<Tensor<B, 5>>,
    constant88: burn::module::Param<Tensor<B, 5>>,
    conv2d34: Conv2d<B>,
    conv2d35: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule4<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d29 = Conv2dConfig::new([192, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let resize3 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([6, 10]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        let constant101: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant100: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d30 = Conv2dConfig::new([96, 193], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d31 = Conv2dConfig::new([96, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant99: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant98: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d32 = Conv2dConfig::new([96, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d33 = Conv2dConfig::new([192, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant89: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant88: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d34 = Conv2dConfig::new([96, 193], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d35 = Conv2dConfig::new([96, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            conv2d29,
            resize3,
            constant101,
            constant100,
            conv2d30,
            conv2d31,
            constant99,
            constant98,
            conv2d32,
            conv2d33,
            constant89,
            constant88,
            conv2d34,
            conv2d35,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, mul47_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 4> {
        let conv2d29_out1 = self.conv2d29.forward(mul47_out1);
        let resize3_out1 = self.resize3.forward(conv2d29_out1);
        let transpose18_out1 = resize3_out1.permute([0, 2, 3, 1]);
        let reshape15_out1 = transpose18_out1.reshape([-1, 3, 2, 5, 2, 96]);
        let transpose19_out1 = reshape15_out1.permute([0, 2, 4, 1, 3, 5]);
        let reshape16_out1 = transpose19_out1.reshape([-1, 4, 15, 96]);
        let reshape17_out1 = reshape16_out1.clone().reshape([-1, 4, 15, 1, 96]);
        let reducemean11_out1 = {
            reshape17_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg6_out1 = reducemean11_out1.clone().neg();
        let sub6_out1 = reshape17_out1.clone().sub(reducemean11_out1);
        let mul48_out1 = sub6_out1.clone().mul(sub6_out1);
        let reducemean12_out1 = {
            mul48_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add28_out1 = reducemean12_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt6_out1 = add28_out1.sqrt();
        let reciprocal6_out1 = sqrt6_out1.recip();
        let constant101_out1 = self.constant101.val();
        let mul49_out1 = reciprocal6_out1.mul(constant101_out1);
        let mul50_out1 = reshape17_out1.mul(mul49_out1.clone());
        let mul51_out1 = neg6_out1.mul(mul49_out1);
        let constant100_out1 = self.constant100.val();
        let add29_out1 = mul51_out1.add(constant100_out1);
        let add30_out1 = mul50_out1.add(add29_out1);
        let reshape18_out1 = add30_out1.reshape([-1, 4, 15, 96]);
        let transpose20_out1 = reshape18_out1.permute([0, 3, 1, 2]);
        let conv2d30_out1 = self.conv2d30.forward(transpose20_out1);
        let transpose21_out1 = conv2d30_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose21_out1.split_with_sizes([1, 96, 96].into(), 3);
        let [split3_out1, split3_out2, split3_out3] = split_tensors.try_into().unwrap();
        let relu3_out1 = burn::tensor::activation::relu(split3_out3);
        let softmax3_out1 = burn::tensor::activation::softmax(split3_out1, 2);
        let mul52_out1 = split3_out2.mul(softmax3_out1);
        let reducesum3_out1 = { mul52_out1.sum_dim(2usize) };
        let mul53_out1 = relu3_out1.mul(reducesum3_out1);
        let transpose22_out1 = mul53_out1.permute([0, 3, 1, 2]);
        let conv2d31_out1 = self.conv2d31.forward(transpose22_out1);
        let transpose23_out1 = reshape16_out1.permute([0, 3, 1, 2]);
        let add31_out1 = transpose23_out1.add(conv2d31_out1);
        let transpose24_out1 = add31_out1.clone().permute([0, 2, 3, 1]);
        let reshape19_out1 = transpose24_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean13_out1 = {
            reshape19_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg7_out1 = reducemean13_out1.clone().neg();
        let sub7_out1 = reshape19_out1.clone().sub(reducemean13_out1);
        let mul54_out1 = sub7_out1.clone().mul(sub7_out1);
        let reducemean14_out1 = {
            mul54_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add32_out1 = reducemean14_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt7_out1 = add32_out1.sqrt();
        let reciprocal7_out1 = sqrt7_out1.recip();
        let constant99_out1 = self.constant99.val();
        let mul55_out1 = reciprocal7_out1.mul(constant99_out1);
        let mul56_out1 = reshape19_out1.mul(mul55_out1.clone());
        let mul57_out1 = neg7_out1.mul(mul55_out1);
        let constant98_out1 = self.constant98.val();
        let add33_out1 = mul57_out1.add(constant98_out1);
        let add34_out1 = mul56_out1.add(add33_out1);
        let reshape20_out1 = add34_out1.reshape([-1, 4, 15, 96]);
        let transpose25_out1 = reshape20_out1.permute([0, 3, 1, 2]);
        let conv2d32_out1 = self.conv2d32.forward(transpose25_out1);
        let sigmoid16_out1 = burn::tensor::activation::sigmoid(conv2d32_out1.clone());
        let mul58_out1 = conv2d32_out1.mul(sigmoid16_out1);
        let conv2d33_out1 = self.conv2d33.forward(mul58_out1);
        let add35_out1 = add31_out1.add(conv2d33_out1);
        let transpose26_out1 = add35_out1.clone().permute([0, 2, 3, 1]);
        let reshape21_out1 = transpose26_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean15_out1 = {
            reshape21_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg8_out1 = reducemean15_out1.clone().neg();
        let sub8_out1 = reshape21_out1.clone().sub(reducemean15_out1);
        let mul59_out1 = sub8_out1.clone().mul(sub8_out1);
        let reducemean16_out1 = {
            mul59_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add36_out1 = reducemean16_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt8_out1 = add36_out1.sqrt();
        let reciprocal8_out1 = sqrt8_out1.recip();
        let constant89_out1 = self.constant89.val();
        let mul60_out1 = reciprocal8_out1.mul(constant89_out1);
        let mul61_out1 = reshape21_out1.mul(mul60_out1.clone());
        let mul62_out1 = neg8_out1.mul(mul60_out1);
        let constant88_out1 = self.constant88.val();
        let add37_out1 = mul62_out1.add(constant88_out1);
        let add38_out1 = mul61_out1.add(add37_out1);
        let reshape22_out1 = add38_out1.reshape([-1, 4, 15, 96]);
        let transpose27_out1 = reshape22_out1.permute([0, 3, 1, 2]);
        let conv2d34_out1 = self.conv2d34.forward(transpose27_out1);
        let transpose28_out1 = conv2d34_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose28_out1.split_with_sizes([1, 96, 96].into(), 3);
        let [split4_out1, split4_out2, split4_out3] = split_tensors.try_into().unwrap();
        let relu4_out1 = burn::tensor::activation::relu(split4_out3);
        let softmax4_out1 = burn::tensor::activation::softmax(split4_out1, 2);
        let mul63_out1 = split4_out2.mul(softmax4_out1);
        let reducesum4_out1 = { mul63_out1.sum_dim(2usize) };
        let mul64_out1 = relu4_out1.mul(reducesum4_out1);
        let transpose29_out1 = mul64_out1.permute([0, 3, 1, 2]);
        let conv2d35_out1 = self.conv2d35.forward(transpose29_out1);
        let add39_out1 = add35_out1.add(conv2d35_out1);
        add39_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule5<B: Backend> {
    constant87: burn::module::Param<Tensor<B, 5>>,
    constant86: burn::module::Param<Tensor<B, 5>>,
    conv2d36: Conv2d<B>,
    conv2d37: Conv2d<B>,
    constant77: burn::module::Param<Tensor<B, 5>>,
    constant76: burn::module::Param<Tensor<B, 5>>,
    conv2d38: Conv2d<B>,
    conv2d39: Conv2d<B>,
    constant75: burn::module::Param<Tensor<B, 5>>,
    constant74: burn::module::Param<Tensor<B, 5>>,
    conv2d40: Conv2d<B>,
    conv2d41: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule5<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant87: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant86: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d36 = Conv2dConfig::new([96, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d37 = Conv2dConfig::new([192, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant77: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant76: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d38 = Conv2dConfig::new([96, 193], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d39 = Conv2dConfig::new([96, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant75: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant74: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d40 = Conv2dConfig::new([96, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d41 = Conv2dConfig::new([192, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            constant87,
            constant86,
            conv2d36,
            conv2d37,
            constant77,
            constant76,
            conv2d38,
            conv2d39,
            constant75,
            constant74,
            conv2d40,
            conv2d41,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add39_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 4> {
        let transpose30_out1 = add39_out1.clone().permute([0, 2, 3, 1]);
        let reshape23_out1 = transpose30_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean17_out1 = {
            reshape23_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg9_out1 = reducemean17_out1.clone().neg();
        let sub9_out1 = reshape23_out1.clone().sub(reducemean17_out1);
        let mul65_out1 = sub9_out1.clone().mul(sub9_out1);
        let reducemean18_out1 = {
            mul65_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add40_out1 = reducemean18_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt9_out1 = add40_out1.sqrt();
        let reciprocal9_out1 = sqrt9_out1.recip();
        let constant87_out1 = self.constant87.val();
        let mul66_out1 = reciprocal9_out1.mul(constant87_out1);
        let mul67_out1 = reshape23_out1.mul(mul66_out1.clone());
        let mul68_out1 = neg9_out1.mul(mul66_out1);
        let constant86_out1 = self.constant86.val();
        let add41_out1 = mul68_out1.add(constant86_out1);
        let add42_out1 = mul67_out1.add(add41_out1);
        let reshape24_out1 = add42_out1.reshape([-1, 4, 15, 96]);
        let transpose31_out1 = reshape24_out1.permute([0, 3, 1, 2]);
        let conv2d36_out1 = self.conv2d36.forward(transpose31_out1);
        let sigmoid17_out1 = burn::tensor::activation::sigmoid(conv2d36_out1.clone());
        let mul69_out1 = conv2d36_out1.mul(sigmoid17_out1);
        let conv2d37_out1 = self.conv2d37.forward(mul69_out1);
        let add43_out1 = add39_out1.add(conv2d37_out1);
        let transpose32_out1 = add43_out1.clone().permute([0, 2, 3, 1]);
        let reshape25_out1 = transpose32_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean19_out1 = {
            reshape25_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg10_out1 = reducemean19_out1.clone().neg();
        let sub10_out1 = reshape25_out1.clone().sub(reducemean19_out1);
        let mul70_out1 = sub10_out1.clone().mul(sub10_out1);
        let reducemean20_out1 = {
            mul70_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add44_out1 = reducemean20_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt10_out1 = add44_out1.sqrt();
        let reciprocal10_out1 = sqrt10_out1.recip();
        let constant77_out1 = self.constant77.val();
        let mul71_out1 = reciprocal10_out1.mul(constant77_out1);
        let mul72_out1 = reshape25_out1.mul(mul71_out1.clone());
        let mul73_out1 = neg10_out1.mul(mul71_out1);
        let constant76_out1 = self.constant76.val();
        let add45_out1 = mul73_out1.add(constant76_out1);
        let add46_out1 = mul72_out1.add(add45_out1);
        let reshape26_out1 = add46_out1.reshape([-1, 4, 15, 96]);
        let transpose33_out1 = reshape26_out1.permute([0, 3, 1, 2]);
        let conv2d38_out1 = self.conv2d38.forward(transpose33_out1);
        let transpose34_out1 = conv2d38_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose34_out1.split_with_sizes([1, 96, 96].into(), 3);
        let [split5_out1, split5_out2, split5_out3] = split_tensors.try_into().unwrap();
        let relu5_out1 = burn::tensor::activation::relu(split5_out3);
        let softmax5_out1 = burn::tensor::activation::softmax(split5_out1, 2);
        let mul74_out1 = split5_out2.mul(softmax5_out1);
        let reducesum5_out1 = { mul74_out1.sum_dim(2usize) };
        let mul75_out1 = relu5_out1.mul(reducesum5_out1);
        let transpose35_out1 = mul75_out1.permute([0, 3, 1, 2]);
        let conv2d39_out1 = self.conv2d39.forward(transpose35_out1);
        let add47_out1 = add43_out1.add(conv2d39_out1);
        let transpose36_out1 = add47_out1.clone().permute([0, 2, 3, 1]);
        let reshape27_out1 = transpose36_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean21_out1 = {
            reshape27_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg11_out1 = reducemean21_out1.clone().neg();
        let sub11_out1 = reshape27_out1.clone().sub(reducemean21_out1);
        let mul76_out1 = sub11_out1.clone().mul(sub11_out1);
        let reducemean22_out1 = {
            mul76_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add48_out1 = reducemean22_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt11_out1 = add48_out1.sqrt();
        let reciprocal11_out1 = sqrt11_out1.recip();
        let constant75_out1 = self.constant75.val();
        let mul77_out1 = reciprocal11_out1.mul(constant75_out1);
        let mul78_out1 = reshape27_out1.mul(mul77_out1.clone());
        let mul79_out1 = neg11_out1.mul(mul77_out1);
        let constant74_out1 = self.constant74.val();
        let add49_out1 = mul79_out1.add(constant74_out1);
        let add50_out1 = mul78_out1.add(add49_out1);
        let reshape28_out1 = add50_out1.reshape([-1, 4, 15, 96]);
        let transpose37_out1 = reshape28_out1.permute([0, 3, 1, 2]);
        let conv2d40_out1 = self.conv2d40.forward(transpose37_out1);
        let sigmoid18_out1 = burn::tensor::activation::sigmoid(conv2d40_out1.clone());
        let mul80_out1 = conv2d40_out1.mul(sigmoid18_out1);
        let conv2d41_out1 = self.conv2d41.forward(mul80_out1);
        let add51_out1 = add47_out1.add(conv2d41_out1);
        add51_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule6<B: Backend> {
    constant65: burn::module::Param<Tensor<B, 5>>,
    constant64: burn::module::Param<Tensor<B, 5>>,
    conv2d42: Conv2d<B>,
    conv2d43: Conv2d<B>,
    constant63: burn::module::Param<Tensor<B, 5>>,
    constant62: burn::module::Param<Tensor<B, 5>>,
    conv2d44: Conv2d<B>,
    conv2d45: Conv2d<B>,
    constant111: burn::module::Param<Tensor<B, 5>>,
    constant110: burn::module::Param<Tensor<B, 5>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule6<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant65: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant64: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d42 = Conv2dConfig::new([96, 193], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d43 = Conv2dConfig::new([96, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant63: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant62: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let conv2d44 = Conv2dConfig::new([96, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d45 = Conv2dConfig::new([192, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant111: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        let constant110: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 96], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 96].into(),
        );
        Self {
            constant65,
            constant64,
            conv2d42,
            conv2d43,
            constant63,
            constant62,
            conv2d44,
            conv2d45,
            constant111,
            constant110,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add51_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 5> {
        let transpose38_out1 = add51_out1.clone().permute([0, 2, 3, 1]);
        let reshape29_out1 = transpose38_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean23_out1 = {
            reshape29_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg12_out1 = reducemean23_out1.clone().neg();
        let sub12_out1 = reshape29_out1.clone().sub(reducemean23_out1);
        let mul81_out1 = sub12_out1.clone().mul(sub12_out1);
        let reducemean24_out1 = {
            mul81_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add52_out1 = reducemean24_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt12_out1 = add52_out1.sqrt();
        let reciprocal12_out1 = sqrt12_out1.recip();
        let constant65_out1 = self.constant65.val();
        let mul82_out1 = reciprocal12_out1.mul(constant65_out1);
        let mul83_out1 = reshape29_out1.mul(mul82_out1.clone());
        let mul84_out1 = neg12_out1.mul(mul82_out1);
        let constant64_out1 = self.constant64.val();
        let add53_out1 = mul84_out1.add(constant64_out1);
        let add54_out1 = mul83_out1.add(add53_out1);
        let reshape30_out1 = add54_out1.reshape([-1, 4, 15, 96]);
        let transpose39_out1 = reshape30_out1.permute([0, 3, 1, 2]);
        let conv2d42_out1 = self.conv2d42.forward(transpose39_out1);
        let transpose40_out1 = conv2d42_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose40_out1.split_with_sizes([1, 96, 96].into(), 3);
        let [split6_out1, split6_out2, split6_out3] = split_tensors.try_into().unwrap();
        let relu6_out1 = burn::tensor::activation::relu(split6_out3);
        let softmax6_out1 = burn::tensor::activation::softmax(split6_out1, 2);
        let mul85_out1 = split6_out2.mul(softmax6_out1);
        let reducesum6_out1 = { mul85_out1.sum_dim(2usize) };
        let mul86_out1 = relu6_out1.mul(reducesum6_out1);
        let transpose41_out1 = mul86_out1.permute([0, 3, 1, 2]);
        let conv2d43_out1 = self.conv2d43.forward(transpose41_out1);
        let add55_out1 = add51_out1.add(conv2d43_out1);
        let transpose42_out1 = add55_out1.clone().permute([0, 2, 3, 1]);
        let reshape31_out1 = transpose42_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean25_out1 = {
            reshape31_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg13_out1 = reducemean25_out1.clone().neg();
        let sub13_out1 = reshape31_out1.clone().sub(reducemean25_out1);
        let mul87_out1 = sub13_out1.clone().mul(sub13_out1);
        let reducemean26_out1 = {
            mul87_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add56_out1 = reducemean26_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt13_out1 = add56_out1.sqrt();
        let reciprocal13_out1 = sqrt13_out1.recip();
        let constant63_out1 = self.constant63.val();
        let mul88_out1 = reciprocal13_out1.mul(constant63_out1);
        let mul89_out1 = reshape31_out1.mul(mul88_out1.clone());
        let mul90_out1 = neg13_out1.mul(mul88_out1);
        let constant62_out1 = self.constant62.val();
        let add57_out1 = mul90_out1.add(constant62_out1);
        let add58_out1 = mul89_out1.add(add57_out1);
        let reshape32_out1 = add58_out1.reshape([-1, 4, 15, 96]);
        let transpose43_out1 = reshape32_out1.permute([0, 3, 1, 2]);
        let conv2d44_out1 = self.conv2d44.forward(transpose43_out1);
        let sigmoid19_out1 = burn::tensor::activation::sigmoid(conv2d44_out1.clone());
        let mul91_out1 = conv2d44_out1.mul(sigmoid19_out1);
        let conv2d45_out1 = self.conv2d45.forward(mul91_out1);
        let add59_out1 = add55_out1.add(conv2d45_out1);
        let transpose44_out1 = add59_out1.permute([0, 2, 3, 1]);
        let reshape33_out1 = transpose44_out1.reshape([-1, 4, 15, 1, 96]);
        let reducemean27_out1 = {
            reshape33_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg14_out1 = reducemean27_out1.clone().neg();
        let sub14_out1 = reshape33_out1.clone().sub(reducemean27_out1);
        let mul92_out1 = sub14_out1.clone().mul(sub14_out1);
        let reducemean28_out1 = {
            mul92_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add60_out1 = reducemean28_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt14_out1 = add60_out1.sqrt();
        let reciprocal14_out1 = sqrt14_out1.recip();
        let constant111_out1 = self.constant111.val();
        let mul93_out1 = reciprocal14_out1.mul(constant111_out1);
        let mul94_out1 = reshape33_out1.mul(mul93_out1.clone());
        let mul95_out1 = neg14_out1.mul(mul93_out1);
        let constant110_out1 = self.constant110.val();
        let add61_out1 = mul95_out1.add(constant110_out1);
        let add62_out1 = mul94_out1.add(add61_out1);
        add62_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule7<B: Backend> {
    resize4: burn::nn::interpolate::Interpolate2d,
    conv2d46: Conv2d<B>,
    conv2d47: Conv2d<B>,
    conv2d48: Conv2d<B>,
    constant61: burn::module::Param<Tensor<B, 4>>,
    constant196: burn::module::Param<Tensor<B, 4>>,
    conv2d49: Conv2d<B>,
    conv2d50: Conv2d<B>,
    constant55: burn::module::Param<Tensor<B, 4>>,
    constant194: burn::module::Param<Tensor<B, 4>>,
    conv2d51: Conv2d<B>,
    resize5: burn::nn::interpolate::Interpolate2d,
    constant42: burn::module::Param<Tensor<B, 5>>,
    constant41: burn::module::Param<Tensor<B, 5>>,
    conv2d52: Conv2d<B>,
    conv2d53: Conv2d<B>,
    constant40: burn::module::Param<Tensor<B, 5>>,
    constant39: burn::module::Param<Tensor<B, 5>>,
    conv2d54: Conv2d<B>,
    conv2d55: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule7<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let resize4 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([5, 9]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        let conv2d46 = Conv2dConfig::new([96, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d47 = Conv2dConfig::new([192, 384], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d48 = Conv2dConfig::new([384, 384], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(384)
            .with_bias(false)
            .init(device);
        let constant61: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 384, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 384, 1, 1].into(),
        );
        let constant196: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 384, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 384, 1, 1].into(),
        );
        let conv2d49 = Conv2dConfig::new([384, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d50 = Conv2dConfig::new([256, 256], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(256)
            .with_bias(false)
            .init(device);
        let constant55: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 256, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 256, 1, 1].into(),
        );
        let constant194: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 4>::zeros([1, 256, 1, 1], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 256, 1, 1].into(),
        );
        let conv2d51 = Conv2dConfig::new([256, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let resize5 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([4, 6]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        let constant42: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant41: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d52 = Conv2dConfig::new([128, 257], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d53 = Conv2dConfig::new([128, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant40: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant39: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d54 = Conv2dConfig::new([128, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d55 = Conv2dConfig::new([256, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            resize4,
            conv2d46,
            conv2d47,
            conv2d48,
            constant61,
            constant196,
            conv2d49,
            conv2d50,
            constant55,
            constant194,
            conv2d51,
            resize5,
            constant42,
            constant41,
            conv2d52,
            conv2d53,
            constant40,
            constant39,
            conv2d54,
            conv2d55,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add62_out1: Tensor<B, 5>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 4> {
        let reshape34_out1 = add62_out1.reshape([-1, 2, 2, 3, 5, 96]);
        let transpose45_out1 = reshape34_out1.permute([0, 3, 1, 4, 2, 5]);
        let reshape35_out1 = transpose45_out1.reshape([-1, 6, 10, 96]);
        let transpose46_out1 = reshape35_out1.permute([0, 3, 1, 2]);
        let resize4_out1 = self.resize4.forward(transpose46_out1);
        let conv2d46_out1 = self.conv2d46.forward(resize4_out1);
        let conv2d47_out1 = self.conv2d47.forward(conv2d46_out1);
        let sigmoid20_out1 = burn::tensor::activation::sigmoid(conv2d47_out1.clone());
        let mul96_out1 = conv2d47_out1.mul(sigmoid20_out1);
        let conv2d48_out1 = self.conv2d48.forward(mul96_out1);
        let constant61_out1 = self.constant61.val();
        let mul97_out1 = conv2d48_out1.mul(constant61_out1);
        let constant196_out1 = self.constant196.val();
        let add63_out1 = mul97_out1.add(constant196_out1);
        let sigmoid21_out1 = burn::tensor::activation::sigmoid(add63_out1.clone());
        let mul98_out1 = add63_out1.mul(sigmoid21_out1);
        let conv2d49_out1 = self.conv2d49.forward(mul98_out1);
        let conv2d50_out1 = self.conv2d50.forward(conv2d49_out1);
        let constant55_out1 = self.constant55.val();
        let mul99_out1 = conv2d50_out1.mul(constant55_out1);
        let constant194_out1 = self.constant194.val();
        let add64_out1 = mul99_out1.add(constant194_out1);
        let sigmoid22_out1 = burn::tensor::activation::sigmoid(add64_out1.clone());
        let mul100_out1 = add64_out1.mul(sigmoid22_out1);
        let conv2d51_out1 = self.conv2d51.forward(mul100_out1);
        let resize5_out1 = self.resize5.forward(conv2d51_out1);
        let transpose47_out1 = resize5_out1.permute([0, 2, 3, 1]);
        let reshape36_out1 = transpose47_out1.reshape([-1, 2, 2, 3, 2, 128]);
        let transpose48_out1 = reshape36_out1.permute([0, 2, 4, 1, 3, 5]);
        let reshape37_out1 = transpose48_out1.reshape([-1, 4, 6, 128]);
        let reshape38_out1 = reshape37_out1.clone().reshape([-1, 4, 6, 1, 128]);
        let reducemean29_out1 = {
            reshape38_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg15_out1 = reducemean29_out1.clone().neg();
        let sub15_out1 = reshape38_out1.clone().sub(reducemean29_out1);
        let mul101_out1 = sub15_out1.clone().mul(sub15_out1);
        let reducemean30_out1 = {
            mul101_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add65_out1 = reducemean30_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt15_out1 = add65_out1.sqrt();
        let reciprocal15_out1 = sqrt15_out1.recip();
        let constant42_out1 = self.constant42.val();
        let mul102_out1 = reciprocal15_out1.mul(constant42_out1);
        let mul103_out1 = reshape38_out1.mul(mul102_out1.clone());
        let mul104_out1 = neg15_out1.mul(mul102_out1);
        let constant41_out1 = self.constant41.val();
        let add66_out1 = mul104_out1.add(constant41_out1);
        let add67_out1 = mul103_out1.add(add66_out1);
        let reshape39_out1 = add67_out1.reshape([-1, 4, 6, 128]);
        let transpose49_out1 = reshape39_out1.permute([0, 3, 1, 2]);
        let conv2d52_out1 = self.conv2d52.forward(transpose49_out1);
        let transpose50_out1 = conv2d52_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose50_out1.split_with_sizes([1, 128, 128].into(), 3);
        let [split7_out1, split7_out2, split7_out3] = split_tensors.try_into().unwrap();
        let relu7_out1 = burn::tensor::activation::relu(split7_out3);
        let softmax7_out1 = burn::tensor::activation::softmax(split7_out1, 2);
        let mul105_out1 = split7_out2.mul(softmax7_out1);
        let reducesum7_out1 = { mul105_out1.sum_dim(2usize) };
        let mul106_out1 = relu7_out1.mul(reducesum7_out1);
        let transpose51_out1 = mul106_out1.permute([0, 3, 1, 2]);
        let conv2d53_out1 = self.conv2d53.forward(transpose51_out1);
        let transpose52_out1 = reshape37_out1.permute([0, 3, 1, 2]);
        let add68_out1 = transpose52_out1.add(conv2d53_out1);
        let transpose53_out1 = add68_out1.clone().permute([0, 2, 3, 1]);
        let reshape40_out1 = transpose53_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean31_out1 = {
            reshape40_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg16_out1 = reducemean31_out1.clone().neg();
        let sub16_out1 = reshape40_out1.clone().sub(reducemean31_out1);
        let mul107_out1 = sub16_out1.clone().mul(sub16_out1);
        let reducemean32_out1 = {
            mul107_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add69_out1 = reducemean32_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt16_out1 = add69_out1.sqrt();
        let reciprocal16_out1 = sqrt16_out1.recip();
        let constant40_out1 = self.constant40.val();
        let mul108_out1 = reciprocal16_out1.mul(constant40_out1);
        let mul109_out1 = reshape40_out1.mul(mul108_out1.clone());
        let mul110_out1 = neg16_out1.mul(mul108_out1);
        let constant39_out1 = self.constant39.val();
        let add70_out1 = mul110_out1.add(constant39_out1);
        let add71_out1 = mul109_out1.add(add70_out1);
        let reshape41_out1 = add71_out1.reshape([-1, 4, 6, 128]);
        let transpose54_out1 = reshape41_out1.permute([0, 3, 1, 2]);
        let conv2d54_out1 = self.conv2d54.forward(transpose54_out1);
        let sigmoid23_out1 = burn::tensor::activation::sigmoid(conv2d54_out1.clone());
        let mul111_out1 = conv2d54_out1.mul(sigmoid23_out1);
        let conv2d55_out1 = self.conv2d55.forward(mul111_out1);
        let add72_out1 = add68_out1.add(conv2d55_out1);
        add72_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule8<B: Backend> {
    constant30: burn::module::Param<Tensor<B, 5>>,
    constant29: burn::module::Param<Tensor<B, 5>>,
    conv2d56: Conv2d<B>,
    conv2d57: Conv2d<B>,
    constant28: burn::module::Param<Tensor<B, 5>>,
    constant27: burn::module::Param<Tensor<B, 5>>,
    conv2d58: Conv2d<B>,
    conv2d59: Conv2d<B>,
    constant18: burn::module::Param<Tensor<B, 5>>,
    constant17: burn::module::Param<Tensor<B, 5>>,
    conv2d60: Conv2d<B>,
    conv2d61: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule8<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant30: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant29: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d56 = Conv2dConfig::new([128, 257], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d57 = Conv2dConfig::new([128, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant28: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant27: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d58 = Conv2dConfig::new([128, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d59 = Conv2dConfig::new([256, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant18: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant17: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d60 = Conv2dConfig::new([128, 257], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d61 = Conv2dConfig::new([128, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            constant30,
            constant29,
            conv2d56,
            conv2d57,
            constant28,
            constant27,
            conv2d58,
            conv2d59,
            constant18,
            constant17,
            conv2d60,
            conv2d61,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add72_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 4> {
        let transpose55_out1 = add72_out1.clone().permute([0, 2, 3, 1]);
        let reshape42_out1 = transpose55_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean33_out1 = {
            reshape42_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg17_out1 = reducemean33_out1.clone().neg();
        let sub17_out1 = reshape42_out1.clone().sub(reducemean33_out1);
        let mul112_out1 = sub17_out1.clone().mul(sub17_out1);
        let reducemean34_out1 = {
            mul112_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add73_out1 = reducemean34_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt17_out1 = add73_out1.sqrt();
        let reciprocal17_out1 = sqrt17_out1.recip();
        let constant30_out1 = self.constant30.val();
        let mul113_out1 = reciprocal17_out1.mul(constant30_out1);
        let mul114_out1 = reshape42_out1.mul(mul113_out1.clone());
        let mul115_out1 = neg17_out1.mul(mul113_out1);
        let constant29_out1 = self.constant29.val();
        let add74_out1 = mul115_out1.add(constant29_out1);
        let add75_out1 = mul114_out1.add(add74_out1);
        let reshape43_out1 = add75_out1.reshape([-1, 4, 6, 128]);
        let transpose56_out1 = reshape43_out1.permute([0, 3, 1, 2]);
        let conv2d56_out1 = self.conv2d56.forward(transpose56_out1);
        let transpose57_out1 = conv2d56_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose57_out1.split_with_sizes([1, 128, 128].into(), 3);
        let [split8_out1, split8_out2, split8_out3] = split_tensors.try_into().unwrap();
        let relu8_out1 = burn::tensor::activation::relu(split8_out3);
        let softmax8_out1 = burn::tensor::activation::softmax(split8_out1, 2);
        let mul116_out1 = split8_out2.mul(softmax8_out1);
        let reducesum8_out1 = { mul116_out1.sum_dim(2usize) };
        let mul117_out1 = relu8_out1.mul(reducesum8_out1);
        let transpose58_out1 = mul117_out1.permute([0, 3, 1, 2]);
        let conv2d57_out1 = self.conv2d57.forward(transpose58_out1);
        let add76_out1 = add72_out1.add(conv2d57_out1);
        let transpose59_out1 = add76_out1.clone().permute([0, 2, 3, 1]);
        let reshape44_out1 = transpose59_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean35_out1 = {
            reshape44_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg18_out1 = reducemean35_out1.clone().neg();
        let sub18_out1 = reshape44_out1.clone().sub(reducemean35_out1);
        let mul118_out1 = sub18_out1.clone().mul(sub18_out1);
        let reducemean36_out1 = {
            mul118_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add77_out1 = reducemean36_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt18_out1 = add77_out1.sqrt();
        let reciprocal18_out1 = sqrt18_out1.recip();
        let constant28_out1 = self.constant28.val();
        let mul119_out1 = reciprocal18_out1.mul(constant28_out1);
        let mul120_out1 = reshape44_out1.mul(mul119_out1.clone());
        let mul121_out1 = neg18_out1.mul(mul119_out1);
        let constant27_out1 = self.constant27.val();
        let add78_out1 = mul121_out1.add(constant27_out1);
        let add79_out1 = mul120_out1.add(add78_out1);
        let reshape45_out1 = add79_out1.reshape([-1, 4, 6, 128]);
        let transpose60_out1 = reshape45_out1.permute([0, 3, 1, 2]);
        let conv2d58_out1 = self.conv2d58.forward(transpose60_out1);
        let sigmoid24_out1 = burn::tensor::activation::sigmoid(conv2d58_out1.clone());
        let mul122_out1 = conv2d58_out1.mul(sigmoid24_out1);
        let conv2d59_out1 = self.conv2d59.forward(mul122_out1);
        let add80_out1 = add76_out1.add(conv2d59_out1);
        let transpose61_out1 = add80_out1.clone().permute([0, 2, 3, 1]);
        let reshape46_out1 = transpose61_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean37_out1 = {
            reshape46_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg19_out1 = reducemean37_out1.clone().neg();
        let sub19_out1 = reshape46_out1.clone().sub(reducemean37_out1);
        let mul123_out1 = sub19_out1.clone().mul(sub19_out1);
        let reducemean38_out1 = {
            mul123_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add81_out1 = reducemean38_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt19_out1 = add81_out1.sqrt();
        let reciprocal19_out1 = sqrt19_out1.recip();
        let constant18_out1 = self.constant18.val();
        let mul124_out1 = reciprocal19_out1.mul(constant18_out1);
        let mul125_out1 = reshape46_out1.mul(mul124_out1.clone());
        let mul126_out1 = neg19_out1.mul(mul124_out1);
        let constant17_out1 = self.constant17.val();
        let add82_out1 = mul126_out1.add(constant17_out1);
        let add83_out1 = mul125_out1.add(add82_out1);
        let reshape47_out1 = add83_out1.reshape([-1, 4, 6, 128]);
        let transpose62_out1 = reshape47_out1.permute([0, 3, 1, 2]);
        let conv2d60_out1 = self.conv2d60.forward(transpose62_out1);
        let transpose63_out1 = conv2d60_out1.permute([0, 2, 3, 1]);
        let split_tensors = transpose63_out1.split_with_sizes([1, 128, 128].into(), 3);
        let [split9_out1, split9_out2, split9_out3] = split_tensors.try_into().unwrap();
        let relu9_out1 = burn::tensor::activation::relu(split9_out3);
        let softmax9_out1 = burn::tensor::activation::softmax(split9_out1, 2);
        let mul127_out1 = split9_out2.mul(softmax9_out1);
        let reducesum9_out1 = { mul127_out1.sum_dim(2usize) };
        let mul128_out1 = relu9_out1.mul(reducesum9_out1);
        let transpose64_out1 = mul128_out1.permute([0, 3, 1, 2]);
        let conv2d61_out1 = self.conv2d61.forward(transpose64_out1);
        let add84_out1 = add80_out1.add(conv2d61_out1);
        add84_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule9<B: Backend> {
    constant16: burn::module::Param<Tensor<B, 5>>,
    constant15: burn::module::Param<Tensor<B, 5>>,
    conv2d62: Conv2d<B>,
    conv2d63: Conv2d<B>,
    constant52: burn::module::Param<Tensor<B, 5>>,
    constant51: burn::module::Param<Tensor<B, 5>>,
    resize6: burn::nn::interpolate::Interpolate2d,
    conv2d64: Conv2d<B>,
    linear1: Linear<B>,
    linear2: Linear<B>,
    linear3: Linear<B>,
    linear4: Linear<B>,
    linear5: Linear<B>,
    linear6: Linear<B>,
    linear7: Linear<B>,
    linear8: Linear<B>,
    linear9: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule9<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant16: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant15: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let conv2d62 = Conv2dConfig::new([128, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d63 = Conv2dConfig::new([256, 128], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant52: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let constant51: burn::module::Param<Tensor<B, 5>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| {
                Tensor::<B, 5>::zeros([1, 1, 1, 1, 128], (device, burn::tensor::DType::F32))
            },
            device.clone(),
            false,
            [1, 1, 1, 1, 128].into(),
        );
        let resize6 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(Some([3, 5]))
            .with_scale_factor(None)
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(false)
            .init();
        let conv2d64 = Conv2dConfig::new([128, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let linear1 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear2 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear3 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear4 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear5 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear6 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear7 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear8 = LinearConfig::new(256, 37).with_bias(true).init(device);
        let linear9 = LinearConfig::new(256, 37).with_bias(true).init(device);
        Self {
            constant16,
            constant15,
            conv2d62,
            conv2d63,
            constant52,
            constant51,
            resize6,
            conv2d64,
            linear1,
            linear2,
            linear3,
            linear4,
            linear5,
            linear6,
            linear7,
            linear8,
            linear9,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add84_out1: Tensor<B, 4>, constant14_out1: Tensor<B, 1>) -> Tensor<B, 2> {
        let transpose65_out1 = add84_out1.clone().permute([0, 2, 3, 1]);
        let reshape48_out1 = transpose65_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean39_out1 = {
            reshape48_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg20_out1 = reducemean39_out1.clone().neg();
        let sub20_out1 = reshape48_out1.clone().sub(reducemean39_out1);
        let mul129_out1 = sub20_out1.clone().mul(sub20_out1);
        let reducemean40_out1 = {
            mul129_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add85_out1 = reducemean40_out1
            .add((constant14_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt20_out1 = add85_out1.sqrt();
        let reciprocal20_out1 = sqrt20_out1.recip();
        let constant16_out1 = self.constant16.val();
        let mul130_out1 = reciprocal20_out1.mul(constant16_out1);
        let mul131_out1 = reshape48_out1.mul(mul130_out1.clone());
        let mul132_out1 = neg20_out1.mul(mul130_out1);
        let constant15_out1 = self.constant15.val();
        let add86_out1 = mul132_out1.add(constant15_out1);
        let add87_out1 = mul131_out1.add(add86_out1);
        let reshape49_out1 = add87_out1.reshape([-1, 4, 6, 128]);
        let transpose66_out1 = reshape49_out1.permute([0, 3, 1, 2]);
        let conv2d62_out1 = self.conv2d62.forward(transpose66_out1);
        let sigmoid25_out1 = burn::tensor::activation::sigmoid(conv2d62_out1.clone());
        let mul133_out1 = conv2d62_out1.mul(sigmoid25_out1);
        let conv2d63_out1 = self.conv2d63.forward(mul133_out1);
        let add88_out1 = add84_out1.add(conv2d63_out1);
        let transpose67_out1 = add88_out1.permute([0, 2, 3, 1]);
        let reshape50_out1 = transpose67_out1.reshape([-1, 4, 6, 1, 128]);
        let reducemean41_out1 = {
            reshape50_out1
                .clone()
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let neg21_out1 = reducemean41_out1.clone().neg();
        let sub21_out1 = reshape50_out1.clone().sub(reducemean41_out1);
        let mul134_out1 = sub21_out1.clone().mul(sub21_out1);
        let reducemean42_out1 = {
            mul134_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .mean_dim(4usize)
        };
        let add89_out1 = reducemean42_out1
            .add((constant14_out1).unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize]));
        let sqrt21_out1 = add89_out1.sqrt();
        let reciprocal21_out1 = sqrt21_out1.recip();
        let constant52_out1 = self.constant52.val();
        let mul135_out1 = reciprocal21_out1.mul(constant52_out1);
        let mul136_out1 = reshape50_out1.mul(mul135_out1.clone());
        let mul137_out1 = neg21_out1.mul(mul135_out1);
        let constant51_out1 = self.constant51.val();
        let add90_out1 = mul137_out1.add(constant51_out1);
        let add91_out1 = mul136_out1.add(add90_out1);
        let reshape51_out1 = add91_out1.reshape([-1, 2, 2, 2, 3, 128]);
        let transpose68_out1 = reshape51_out1.permute([0, 3, 1, 4, 2, 5]);
        let reshape52_out1 = transpose68_out1.reshape([-1, 4, 6, 128]);
        let transpose69_out1 = reshape52_out1.permute([0, 3, 1, 2]);
        let resize6_out1 = self.resize6.forward(transpose69_out1);
        let conv2d64_out1 = self.conv2d64.forward(resize6_out1);
        let transpose70_out1 = conv2d64_out1.permute([0, 2, 3, 1]);
        let reducemean43_out1 = {
            transpose70_out1
                .mean_dim(1usize)
                .mean_dim(2usize)
                .squeeze_dims::<2usize>(&[1, 2])
        };
        let linear1_out1 = self.linear1.forward(reducemean43_out1.clone());
        let softmax10_out1 = burn::tensor::activation::softmax(linear1_out1, 1);
        let linear2_out1 = self.linear2.forward(reducemean43_out1.clone());
        let softmax11_out1 = burn::tensor::activation::softmax(linear2_out1, 1);
        let linear3_out1 = self.linear3.forward(reducemean43_out1.clone());
        let softmax12_out1 = burn::tensor::activation::softmax(linear3_out1, 1);
        let linear4_out1 = self.linear4.forward(reducemean43_out1.clone());
        let softmax13_out1 = burn::tensor::activation::softmax(linear4_out1, 1);
        let linear5_out1 = self.linear5.forward(reducemean43_out1.clone());
        let softmax14_out1 = burn::tensor::activation::softmax(linear5_out1, 1);
        let linear6_out1 = self.linear6.forward(reducemean43_out1.clone());
        let softmax15_out1 = burn::tensor::activation::softmax(linear6_out1, 1);
        let linear7_out1 = self.linear7.forward(reducemean43_out1.clone());
        let softmax16_out1 = burn::tensor::activation::softmax(linear7_out1, 1);
        let linear8_out1 = self.linear8.forward(reducemean43_out1.clone());
        let softmax17_out1 = burn::tensor::activation::softmax(linear8_out1, 1);
        let linear9_out1 = self.linear9.forward(reducemean43_out1);
        let softmax18_out1 = burn::tensor::activation::softmax(linear9_out1, 1);
        let concat1_out1 = burn::tensor::Tensor::cat(
            [
                softmax10_out1,
                softmax18_out1,
                softmax17_out1,
                softmax16_out1,
                softmax15_out1,
                softmax14_out1,
                softmax13_out1,
                softmax12_out1,
                softmax11_out1,
            ]
            .into(),
            1,
        );
        concat1_out1
    }
}

#[derive(Module, Debug)]
pub struct Model<B: Backend> {
    submodule1: Submodule1<B>,
    submodule2: Submodule2<B>,
    submodule3: Submodule3<B>,
    submodule4: Submodule4<B>,
    submodule5: Submodule5<B>,
    submodule6: Submodule6<B>,
    submodule7: Submodule7<B>,
    submodule8: Submodule8<B>,
    submodule9: Submodule9<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}

extern crate std;

impl<B: Backend> Default for Model<B> {
    fn default() -> Self {
        Self::from_file("plate_ocr.bpk", &Default::default())
    }
}

impl<B: Backend> Model<B> {
    /// Load model weights from a burnpack file.
    pub fn from_file<P: AsRef<std::path::Path>>(file: P, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_file(file);
        model
            .load_from(&mut store)
            .expect("Failed to load burnpack file");
        model
    }

    /// Load model weights from in-memory bytes.
    ///
    /// The bytes must be the contents of a `.bpk` file.
    pub fn from_bytes(bytes: Bytes, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_bytes(Some(bytes));
        model
            .load_from(&mut store)
            .expect("Failed to load burnpack bytes");
        model
    }
}

impl<B: Backend> Model<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let submodule1 = Submodule1::new(device);
        let submodule2 = Submodule2::new(device);
        let submodule3 = Submodule3::new(device);
        let submodule4 = Submodule4::new(device);
        let submodule5 = Submodule5::new(device);
        let submodule6 = Submodule6::new(device);
        let submodule7 = Submodule7::new(device);
        let submodule8 = Submodule8::new(device);
        let submodule9 = Submodule9::new(device);
        Self {
            submodule1,
            submodule2,
            submodule3,
            submodule4,
            submodule5,
            submodule6,
            submodule7,
            submodule8,
            submodule9,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }

    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, input: Tensor<B, 4, Int>) -> Tensor<B, 2> {
        let transpose2_out1 = self.submodule1.forward(input);
        let (add18_out1, constant14_out1) = self.submodule2.forward(transpose2_out1);
        let mul47_out1 = self.submodule3.forward(add18_out1, constant14_out1.clone());
        let add39_out1 = self.submodule4.forward(mul47_out1, constant14_out1.clone());
        let add51_out1 = self.submodule5.forward(add39_out1, constant14_out1.clone());
        let add62_out1 = self.submodule6.forward(add51_out1, constant14_out1.clone());
        let add72_out1 = self.submodule7.forward(add62_out1, constant14_out1.clone());
        let add84_out1 = self.submodule8.forward(add72_out1, constant14_out1.clone());
        let concat1_out1 = self.submodule9.forward(add84_out1, constant14_out1);
        concat1_out1
    }
}
