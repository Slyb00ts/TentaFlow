// Generated from ONNX "/home/critix/repos/rust/TentaFlow/.runtime/models/vision/depth-anything-v2-metric.onnx" by burn-onnx
use burn::prelude::*;
use burn::nn::LayerNorm;
use burn::nn::LayerNormConfig;
use burn::nn::Linear;
use burn::nn::LinearConfig;
use burn::nn::PaddingConfig2d;
use burn::nn::conv::Conv2d;
use burn::nn::conv::Conv2dConfig;
use burn::nn::conv::ConvTranspose2d;
use burn::nn::conv::ConvTranspose2dConfig;
use burn::tensor::Bytes;
use burn_store::BurnpackStore;
use burn_store::ModuleSnapshot;


#[derive(Module, Debug)]
pub struct Submodule1<B: Backend> {
    conv2d1: Conv2d<B>,
    constant295: burn::module::Param<Tensor<B, 1, Int>>,
    constant1: burn::module::Param<Tensor<B, 3>>,
    constant300: burn::module::Param<Tensor<B, 1>>,
    constant299: burn::module::Param<Tensor<B, 1>>,
    constant211: burn::module::Param<Tensor<B, 3>>,
    constant210: burn::module::Param<Tensor<B, 3>>,
    layernormalization1: LayerNorm<B>,
    linear1: Linear<B>,
    linear2: Linear<B>,
    linear3: Linear<B>,
    constant326: burn::module::Param<Tensor<B, 1>>,
    constant327: burn::module::Param<Tensor<B, 1>>,
    linear4: Linear<B>,
    constant10: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule1<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d1 = Conv2dConfig::new([3, 384], [14, 14])
            .with_stride([14, 14])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant295: burn::module::Param<Tensor<B, 1, Int>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
                Int,
            >::from_data(
                burn::tensor::TensorData::from([-1i64]),
                (device, burn::tensor::DType::I64),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1, 384].into(),
        );
        let constant300: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant299: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1369f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant211: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1369, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1369, 384].into(),
        );
        let constant210: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1, 384].into(),
        );
        let layernormalization1 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear1 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear2 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear3 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant326: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant327: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear4 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant10: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            conv2d1,
            constant295,
            constant1,
            constant300,
            constant299,
            constant211,
            constant210,
            layernormalization1,
            linear1,
            linear2,
            linear3,
            constant326,
            constant327,
            linear4,
            constant10,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, pixel_values: Tensor<B, 4>) -> (Tensor<B, 3>, [i64; 4]) {
        let shape1_out1: [i64; 4] = {
            let axes = &pixel_values.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather1_out1 = shape1_out1[0] as i64;
        let gather2_out1 = shape1_out1[2] as i64;
        let gather3_out1 = shape1_out1[3] as i64;
        let conv2d1_out1 = self.conv2d1.forward(pixel_values);
        let shape4_out1: [i64; 4] = {
            let axes = &conv2d1_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice1_out1: [i64; 2] = shape4_out1[0..2].try_into().unwrap();
        let constant290_out1: [i64; 1] = [-1i64];
        let concat1_out1: [i64; 3usize] = [&slice1_out1[..], &constant290_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape1_out1 = conv2d1_out1.reshape(concat1_out1);
        let transpose1_out1 = reshape1_out1.permute([0, 2, 1]);
        let unsqueeze1_out1 = [gather1_out1 as i64];
        let constant293_out1: [i64; 1] = [-1i64];
        let constant292_out1: [i64; 1] = [-1i64];
        let concat2_out1: [i64; 3usize] = [
            &unsqueeze1_out1[..],
            &constant292_out1[..],
            &constant293_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape2_out1 = concat2_out1;
        let shape5_out1: [i64; 1] = [3i64];
        let constantofshape1_out1 = Tensor::<
            B,
            1,
            Int,
        >::from_data(
                burn::tensor::TensorData::from([1i64 as i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .reshape([1])
            .expand(shape5_out1);
        let constant295_out1 = self.constant295.val();
        let mul1_out1 = constantofshape1_out1.clone().mul(constant295_out1);
        let equal1_out1 = {
            let shape_tensor = Tensor::<
                B,
                1,
                Int,
            >::from_data(
                burn::tensor::TensorData::from(reshape2_out1.as_slice()),
                (&self.device, burn::tensor::DType::I64),
            );
            shape_tensor.equal(mul1_out1)
        };
        let where1_out1 = Tensor::<
            B,
            1,
            burn::tensor::Int,
        >::from_data(
                burn::tensor::TensorData::from(&reshape2_out1 as &[i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .mask_where(equal1_out1, constantofshape1_out1);
        let constant1_out1 = self.constant1.val();
        let expand1_out1 = {
            let onnx_shape: [i64; 3usize] = TryInto::<
                [i64; 3usize],
            >::try_into(where1_out1.to_data().convert::<i64>().as_slice().unwrap())
                .unwrap();
            let input_dims = constant1_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            constant1_out1.expand(shape)
        };
        let concat3_out1 = burn::tensor::Tensor::cat(
            [expand1_out1, transpose1_out1].into(),
            1,
        );
        let shape6_out1: [i64; 3] = {
            let axes = &concat3_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather4_out1 = shape6_out1[2] as i64;
        let constant297_out1 = 14i64;
        let div1_out1 = gather2_out1 / constant297_out1;
        let constant298_out1 = 14i64;
        let div2_out1 = gather3_out1 / constant298_out1;
        let constant300_out1 = self.constant300.val();
        let constant299_out1 = self.constant299.val();
        let pow1_out1 = constant299_out1.powf(constant300_out1);
        let cast6_out1 = pow1_out1.int().cast(burn::tensor::DType::I64);
        let unsqueeze2_out1 = {
            let __v: i64 = cast6_out1.clone().into_scalar().elem();
            [__v as i64]
        };
        let unsqueeze3_out1 = {
            let __v: i64 = cast6_out1.into_scalar().elem();
            [__v as i64]
        };
        let unsqueeze4_out1 = [gather4_out1 as i64];
        let constant301_out1: [i64; 1] = [1i64];
        let concat4_out1: [i64; 4usize] = [
            &constant301_out1[..],
            &unsqueeze2_out1[..],
            &unsqueeze3_out1[..],
            &unsqueeze4_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let constant211_out1 = self.constant211.val();
        let reshape3_out1 = constant211_out1.reshape([1i64, 37, 37, 384]);
        let transpose2_out1 = reshape3_out1.permute([0, 3, 1, 2]);
        let unsqueeze5_out1 = [div1_out1 as i64];
        let unsqueeze6_out1 = [div2_out1 as i64];
        let concat5_out1: [i64; 2usize] = [&unsqueeze5_out1[..], &unsqueeze6_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let shape7_out1: [i64; 4] = {
            let axes = &transpose2_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice2_out1: [i64; 2] = shape7_out1[0..2].try_into().unwrap();
        let concat6_out1: [i64; 4usize] = [&slice2_out1[..], &concat5_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let resize1_out1 = {
            let target_height = concat6_out1[2] as usize;
            let target_width = concat6_out1[3] as usize;
            burn::tensor::module::interpolate(
                transpose2_out1,
                [target_height, target_width],
                burn::tensor::ops::InterpolateOptions::new(
                        burn::tensor::ops::InterpolateMode::Bicubic,
                    )
                    .with_align_corners(false),
            )
        };
        let transpose3_out1 = resize1_out1.permute([0, 2, 3, 1]);
        let unsqueeze7_out1 = [gather4_out1 as i64];
        let constant310_out1: [i64; 1] = [1i64];
        let constant311_out1: [i64; 1] = [-1i64];
        let concat7_out1: [i64; 3usize] = [
            &constant310_out1[..],
            &constant311_out1[..],
            &unsqueeze7_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape4_out1 = transpose3_out1.reshape(concat7_out1);
        let constant210_out1 = self.constant210.val();
        let concat8_out1 = burn::tensor::Tensor::cat(
            [constant210_out1, reshape4_out1].into(),
            1,
        );
        let add1_out1 = concat3_out1.add(concat8_out1);
        let layernormalization1_out1 = {
            let dtype = add1_out1.clone().dtype();
            self.layernormalization1
                .forward(add1_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape8_out1: [i64; 3] = {
            let axes = &layernormalization1_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather5_out1 = shape8_out1[0] as i64;
        let linear1_out1 = self.linear1.forward(layernormalization1_out1.clone());
        let unsqueeze8_out1 = [gather5_out1 as i64];
        let constant317_out1: [i64; 1] = [64i64];
        let constant315_out1: [i64; 1] = [-1i64];
        let constant316_out1: [i64; 1] = [6i64];
        let concat9_out1: [i64; 4usize] = [
            &unsqueeze8_out1[..],
            &constant315_out1[..],
            &constant316_out1[..],
            &constant317_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze9_out1 = [gather5_out1 as i64];
        let constant321_out1: [i64; 1] = [64i64];
        let constant319_out1: [i64; 1] = [-1i64];
        let constant320_out1: [i64; 1] = [6i64];
        let concat10_out1: [i64; 4usize] = [
            &unsqueeze9_out1[..],
            &constant319_out1[..],
            &constant320_out1[..],
            &constant321_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze10_out1 = [gather5_out1 as i64];
        let constant325_out1: [i64; 1] = [64i64];
        let constant323_out1: [i64; 1] = [-1i64];
        let constant324_out1: [i64; 1] = [6i64];
        let concat11_out1: [i64; 4usize] = [
            &unsqueeze10_out1[..],
            &constant323_out1[..],
            &constant324_out1[..],
            &constant325_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape5_out1 = linear1_out1.reshape(concat9_out1);
        let linear2_out1 = self.linear2.forward(layernormalization1_out1.clone());
        let reshape6_out1 = linear2_out1.reshape(concat10_out1);
        let transpose4_out1 = reshape6_out1.permute([0, 2, 1, 3]);
        let linear3_out1 = self.linear3.forward(layernormalization1_out1);
        let reshape7_out1 = linear3_out1.reshape(concat11_out1);
        let transpose5_out1 = reshape7_out1.permute([0, 2, 1, 3]);
        let transpose6_out1 = reshape5_out1.permute([0, 2, 3, 1]);
        let constant326_out1 = self.constant326.val();
        let mul2_out1 = transpose5_out1
            .mul((constant326_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant327_out1 = self.constant327.val();
        let mul3_out1 = transpose6_out1
            .mul((constant327_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul4_out1 = mul2_out1.matmul(mul3_out1);
        let softmax1_out1 = burn::tensor::activation::softmax(matmul4_out1, 3);
        let matmul5_out1 = softmax1_out1.matmul(transpose4_out1);
        let transpose7_out1 = matmul5_out1.permute([0, 2, 1, 3]);
        let shape9_out1: [i64; 4] = {
            let axes = &transpose7_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather6_out1 = shape9_out1[0] as i64;
        let gather7_out1 = shape9_out1[1] as i64;
        let unsqueeze11_out1 = [gather6_out1 as i64];
        let unsqueeze12_out1 = [gather7_out1 as i64];
        let constant332_out1: [i64; 1] = [384i64];
        let concat12_out1: [i64; 3usize] = [
            &unsqueeze11_out1[..],
            &unsqueeze12_out1[..],
            &constant332_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape8_out1 = transpose7_out1.reshape(concat12_out1);
        let linear4_out1 = self.linear4.forward(reshape8_out1);
        let constant10_out1 = self.constant10.val();
        let mul4_out1 = linear4_out1
            .mul((constant10_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add2_out1 = mul4_out1.add(add1_out1);
        (add2_out1, shape1_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule2<B: Backend> {
    layernormalization2: LayerNorm<B>,
    linear5: Linear<B>,
    constant333: burn::module::Param<Tensor<B, 1>>,
    constant334: burn::module::Param<Tensor<B, 1>>,
    constant335: burn::module::Param<Tensor<B, 1>>,
    linear6: Linear<B>,
    constant15: burn::module::Param<Tensor<B, 1>>,
    layernormalization3: LayerNorm<B>,
    linear7: Linear<B>,
    linear8: Linear<B>,
    linear9: Linear<B>,
    constant349: burn::module::Param<Tensor<B, 1>>,
    constant350: burn::module::Param<Tensor<B, 1>>,
    linear10: Linear<B>,
    constant22: burn::module::Param<Tensor<B, 1>>,
    layernormalization4: LayerNorm<B>,
    linear11: Linear<B>,
    constant356: burn::module::Param<Tensor<B, 1>>,
    constant357: burn::module::Param<Tensor<B, 1>>,
    constant358: burn::module::Param<Tensor<B, 1>>,
    linear12: Linear<B>,
    constant27: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule2<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization2 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear5 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant333: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant334: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant335: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear6 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant15: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization3 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear7 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear8 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear9 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant349: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant350: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear10 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant22: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization4 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear11 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant356: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant357: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant358: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear12 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant27: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            layernormalization2,
            linear5,
            constant333,
            constant334,
            constant335,
            linear6,
            constant15,
            layernormalization3,
            linear7,
            linear8,
            linear9,
            constant349,
            constant350,
            linear10,
            constant22,
            layernormalization4,
            linear11,
            constant356,
            constant357,
            constant358,
            linear12,
            constant27,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add2_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization2_out1 = {
            let dtype = add2_out1.clone().dtype();
            self.layernormalization2
                .forward(add2_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear5_out1 = self.linear5.forward(layernormalization2_out1);
        let constant333_out1 = self.constant333.val();
        let div3_out1 = linear5_out1
            .clone()
            .div((constant333_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf1_out1 = div3_out1.erf();
        let constant334_out1 = self.constant334.val();
        let add3_out1 = erf1_out1
            .add((constant334_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul5_out1 = linear5_out1.mul(add3_out1);
        let constant335_out1 = self.constant335.val();
        let mul6_out1 = mul5_out1
            .mul((constant335_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear6_out1 = self.linear6.forward(mul6_out1);
        let constant15_out1 = self.constant15.val();
        let mul7_out1 = linear6_out1
            .mul((constant15_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add4_out1 = mul7_out1.add(add2_out1);
        let layernormalization3_out1 = {
            let dtype = add4_out1.clone().dtype();
            self.layernormalization3
                .forward(add4_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape11_out1: [i64; 3] = {
            let axes = &layernormalization3_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather8_out1 = shape11_out1[0] as i64;
        let linear7_out1 = self.linear7.forward(layernormalization3_out1.clone());
        let unsqueeze13_out1 = [gather8_out1 as i64];
        let constant340_out1: [i64; 1] = [64i64];
        let constant338_out1: [i64; 1] = [-1i64];
        let constant339_out1: [i64; 1] = [6i64];
        let concat13_out1: [i64; 4usize] = [
            &unsqueeze13_out1[..],
            &constant338_out1[..],
            &constant339_out1[..],
            &constant340_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze14_out1 = [gather8_out1 as i64];
        let constant344_out1: [i64; 1] = [64i64];
        let constant342_out1: [i64; 1] = [-1i64];
        let constant343_out1: [i64; 1] = [6i64];
        let concat14_out1: [i64; 4usize] = [
            &unsqueeze14_out1[..],
            &constant342_out1[..],
            &constant343_out1[..],
            &constant344_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze15_out1 = [gather8_out1 as i64];
        let constant348_out1: [i64; 1] = [64i64];
        let constant346_out1: [i64; 1] = [-1i64];
        let constant347_out1: [i64; 1] = [6i64];
        let concat15_out1: [i64; 4usize] = [
            &unsqueeze15_out1[..],
            &constant346_out1[..],
            &constant347_out1[..],
            &constant348_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape9_out1 = linear7_out1.reshape(concat13_out1);
        let linear8_out1 = self.linear8.forward(layernormalization3_out1.clone());
        let reshape10_out1 = linear8_out1.reshape(concat14_out1);
        let transpose8_out1 = reshape10_out1.permute([0, 2, 1, 3]);
        let linear9_out1 = self.linear9.forward(layernormalization3_out1);
        let reshape11_out1 = linear9_out1.reshape(concat15_out1);
        let transpose9_out1 = reshape11_out1.permute([0, 2, 1, 3]);
        let transpose10_out1 = reshape9_out1.permute([0, 2, 3, 1]);
        let constant349_out1 = self.constant349.val();
        let mul8_out1 = transpose9_out1
            .mul((constant349_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant350_out1 = self.constant350.val();
        let mul9_out1 = transpose10_out1
            .mul((constant350_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul12_out1 = mul8_out1.matmul(mul9_out1);
        let softmax2_out1 = burn::tensor::activation::softmax(matmul12_out1, 3);
        let matmul13_out1 = softmax2_out1.matmul(transpose8_out1);
        let transpose11_out1 = matmul13_out1.permute([0, 2, 1, 3]);
        let shape12_out1: [i64; 4] = {
            let axes = &transpose11_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather9_out1 = shape12_out1[0] as i64;
        let gather10_out1 = shape12_out1[1] as i64;
        let unsqueeze16_out1 = [gather9_out1 as i64];
        let unsqueeze17_out1 = [gather10_out1 as i64];
        let constant355_out1: [i64; 1] = [384i64];
        let concat16_out1: [i64; 3usize] = [
            &unsqueeze16_out1[..],
            &unsqueeze17_out1[..],
            &constant355_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape12_out1 = transpose11_out1.reshape(concat16_out1);
        let linear10_out1 = self.linear10.forward(reshape12_out1);
        let constant22_out1 = self.constant22.val();
        let mul10_out1 = linear10_out1
            .mul((constant22_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add5_out1 = mul10_out1.add(add4_out1);
        let layernormalization4_out1 = {
            let dtype = add5_out1.clone().dtype();
            self.layernormalization4
                .forward(add5_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear11_out1 = self.linear11.forward(layernormalization4_out1);
        let constant356_out1 = self.constant356.val();
        let div4_out1 = linear11_out1
            .clone()
            .div((constant356_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf2_out1 = div4_out1.erf();
        let constant357_out1 = self.constant357.val();
        let add6_out1 = erf2_out1
            .add((constant357_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul11_out1 = linear11_out1.mul(add6_out1);
        let constant358_out1 = self.constant358.val();
        let mul12_out1 = mul11_out1
            .mul((constant358_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear12_out1 = self.linear12.forward(mul12_out1);
        let constant27_out1 = self.constant27.val();
        let mul13_out1 = linear12_out1
            .mul((constant27_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add7_out1 = mul13_out1.add(add5_out1);
        add7_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule3<B: Backend> {
    layernormalization5: LayerNorm<B>,
    linear13: Linear<B>,
    linear14: Linear<B>,
    linear15: Linear<B>,
    constant372: burn::module::Param<Tensor<B, 1>>,
    constant373: burn::module::Param<Tensor<B, 1>>,
    linear16: Linear<B>,
    constant34: burn::module::Param<Tensor<B, 1>>,
    layernormalization6: LayerNorm<B>,
    linear17: Linear<B>,
    constant379: burn::module::Param<Tensor<B, 1>>,
    constant380: burn::module::Param<Tensor<B, 1>>,
    constant381: burn::module::Param<Tensor<B, 1>>,
    linear18: Linear<B>,
    constant39: burn::module::Param<Tensor<B, 1>>,
    layernormalization7: LayerNorm<B>,
    linear19: Linear<B>,
    linear20: Linear<B>,
    linear21: Linear<B>,
    constant395: burn::module::Param<Tensor<B, 1>>,
    constant396: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule3<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization5 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear13 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear14 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear15 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant372: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant373: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear16 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant34: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization6 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear17 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant379: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant380: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant381: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear18 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant39: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization7 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear19 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear20 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear21 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant395: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant396: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        Self {
            layernormalization5,
            linear13,
            linear14,
            linear15,
            constant372,
            constant373,
            linear16,
            constant34,
            layernormalization6,
            linear17,
            constant379,
            constant380,
            constant381,
            linear18,
            constant39,
            layernormalization7,
            linear19,
            linear20,
            linear21,
            constant395,
            constant396,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add7_out1: Tensor<B, 3>) -> (Tensor<B, 4>, Tensor<B, 3>) {
        let layernormalization5_out1 = {
            let dtype = add7_out1.clone().dtype();
            self.layernormalization5
                .forward(add7_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape14_out1: [i64; 3] = {
            let axes = &layernormalization5_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather11_out1 = shape14_out1[0] as i64;
        let linear13_out1 = self.linear13.forward(layernormalization5_out1.clone());
        let unsqueeze18_out1 = [gather11_out1 as i64];
        let constant363_out1: [i64; 1] = [64i64];
        let constant361_out1: [i64; 1] = [-1i64];
        let constant362_out1: [i64; 1] = [6i64];
        let concat17_out1: [i64; 4usize] = [
            &unsqueeze18_out1[..],
            &constant361_out1[..],
            &constant362_out1[..],
            &constant363_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze19_out1 = [gather11_out1 as i64];
        let constant367_out1: [i64; 1] = [64i64];
        let constant365_out1: [i64; 1] = [-1i64];
        let constant366_out1: [i64; 1] = [6i64];
        let concat18_out1: [i64; 4usize] = [
            &unsqueeze19_out1[..],
            &constant365_out1[..],
            &constant366_out1[..],
            &constant367_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze20_out1 = [gather11_out1 as i64];
        let constant371_out1: [i64; 1] = [64i64];
        let constant369_out1: [i64; 1] = [-1i64];
        let constant370_out1: [i64; 1] = [6i64];
        let concat19_out1: [i64; 4usize] = [
            &unsqueeze20_out1[..],
            &constant369_out1[..],
            &constant370_out1[..],
            &constant371_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape13_out1 = linear13_out1.reshape(concat17_out1);
        let linear14_out1 = self.linear14.forward(layernormalization5_out1.clone());
        let reshape14_out1 = linear14_out1.reshape(concat18_out1);
        let transpose12_out1 = reshape14_out1.permute([0, 2, 1, 3]);
        let linear15_out1 = self.linear15.forward(layernormalization5_out1);
        let reshape15_out1 = linear15_out1.reshape(concat19_out1);
        let transpose13_out1 = reshape15_out1.permute([0, 2, 1, 3]);
        let transpose14_out1 = reshape13_out1.permute([0, 2, 3, 1]);
        let constant372_out1 = self.constant372.val();
        let mul14_out1 = transpose13_out1
            .mul((constant372_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant373_out1 = self.constant373.val();
        let mul15_out1 = transpose14_out1
            .mul((constant373_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul20_out1 = mul14_out1.matmul(mul15_out1);
        let softmax3_out1 = burn::tensor::activation::softmax(matmul20_out1, 3);
        let matmul21_out1 = softmax3_out1.matmul(transpose12_out1);
        let transpose15_out1 = matmul21_out1.permute([0, 2, 1, 3]);
        let shape15_out1: [i64; 4] = {
            let axes = &transpose15_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather12_out1 = shape15_out1[0] as i64;
        let gather13_out1 = shape15_out1[1] as i64;
        let unsqueeze21_out1 = [gather12_out1 as i64];
        let unsqueeze22_out1 = [gather13_out1 as i64];
        let constant378_out1: [i64; 1] = [384i64];
        let concat20_out1: [i64; 3usize] = [
            &unsqueeze21_out1[..],
            &unsqueeze22_out1[..],
            &constant378_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape16_out1 = transpose15_out1.reshape(concat20_out1);
        let linear16_out1 = self.linear16.forward(reshape16_out1);
        let constant34_out1 = self.constant34.val();
        let mul16_out1 = linear16_out1
            .mul((constant34_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add8_out1 = mul16_out1.add(add7_out1);
        let layernormalization6_out1 = {
            let dtype = add8_out1.clone().dtype();
            self.layernormalization6
                .forward(add8_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear17_out1 = self.linear17.forward(layernormalization6_out1);
        let constant379_out1 = self.constant379.val();
        let div5_out1 = linear17_out1
            .clone()
            .div((constant379_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf3_out1 = div5_out1.erf();
        let constant380_out1 = self.constant380.val();
        let add9_out1 = erf3_out1
            .add((constant380_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul17_out1 = linear17_out1.mul(add9_out1);
        let constant381_out1 = self.constant381.val();
        let mul18_out1 = mul17_out1
            .mul((constant381_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear18_out1 = self.linear18.forward(mul18_out1);
        let constant39_out1 = self.constant39.val();
        let mul19_out1 = linear18_out1
            .mul((constant39_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add10_out1 = mul19_out1.add(add8_out1);
        let layernormalization7_out1 = {
            let dtype = add10_out1.clone().dtype();
            self.layernormalization7
                .forward(add10_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape17_out1: [i64; 3] = {
            let axes = &layernormalization7_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather14_out1 = shape17_out1[0] as i64;
        let linear19_out1 = self.linear19.forward(layernormalization7_out1.clone());
        let unsqueeze23_out1 = [gather14_out1 as i64];
        let constant386_out1: [i64; 1] = [64i64];
        let constant384_out1: [i64; 1] = [-1i64];
        let constant385_out1: [i64; 1] = [6i64];
        let concat21_out1: [i64; 4usize] = [
            &unsqueeze23_out1[..],
            &constant384_out1[..],
            &constant385_out1[..],
            &constant386_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze24_out1 = [gather14_out1 as i64];
        let constant390_out1: [i64; 1] = [64i64];
        let constant388_out1: [i64; 1] = [-1i64];
        let constant389_out1: [i64; 1] = [6i64];
        let concat22_out1: [i64; 4usize] = [
            &unsqueeze24_out1[..],
            &constant388_out1[..],
            &constant389_out1[..],
            &constant390_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze25_out1 = [gather14_out1 as i64];
        let constant394_out1: [i64; 1] = [64i64];
        let constant392_out1: [i64; 1] = [-1i64];
        let constant393_out1: [i64; 1] = [6i64];
        let concat23_out1: [i64; 4usize] = [
            &unsqueeze25_out1[..],
            &constant392_out1[..],
            &constant393_out1[..],
            &constant394_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape17_out1 = linear19_out1.reshape(concat21_out1);
        let linear20_out1 = self.linear20.forward(layernormalization7_out1.clone());
        let reshape18_out1 = linear20_out1.reshape(concat22_out1);
        let transpose16_out1 = reshape18_out1.permute([0, 2, 1, 3]);
        let linear21_out1 = self.linear21.forward(layernormalization7_out1);
        let reshape19_out1 = linear21_out1.reshape(concat23_out1);
        let transpose17_out1 = reshape19_out1.permute([0, 2, 1, 3]);
        let transpose18_out1 = reshape17_out1.permute([0, 2, 3, 1]);
        let constant395_out1 = self.constant395.val();
        let mul20_out1 = transpose17_out1
            .mul((constant395_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant396_out1 = self.constant396.val();
        let mul21_out1 = transpose18_out1
            .mul((constant396_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul28_out1 = mul20_out1.matmul(mul21_out1);
        let softmax4_out1 = burn::tensor::activation::softmax(matmul28_out1, 3);
        let matmul29_out1 = softmax4_out1.matmul(transpose16_out1);
        (matmul29_out1, add10_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule4<B: Backend> {
    linear22: Linear<B>,
    constant46: burn::module::Param<Tensor<B, 1>>,
    layernormalization8: LayerNorm<B>,
    linear23: Linear<B>,
    constant402: burn::module::Param<Tensor<B, 1>>,
    constant403: burn::module::Param<Tensor<B, 1>>,
    constant404: burn::module::Param<Tensor<B, 1>>,
    linear24: Linear<B>,
    constant51: burn::module::Param<Tensor<B, 1>>,
    layernormalization9: LayerNorm<B>,
    linear25: Linear<B>,
    linear26: Linear<B>,
    linear27: Linear<B>,
    constant418: burn::module::Param<Tensor<B, 1>>,
    constant419: burn::module::Param<Tensor<B, 1>>,
    linear28: Linear<B>,
    constant58: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule4<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let linear22 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant46: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization8 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear23 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant402: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant403: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant404: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear24 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant51: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization9 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear25 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear26 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear27 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant418: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant419: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear28 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant58: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            linear22,
            constant46,
            layernormalization8,
            linear23,
            constant402,
            constant403,
            constant404,
            linear24,
            constant51,
            layernormalization9,
            linear25,
            linear26,
            linear27,
            constant418,
            constant419,
            linear28,
            constant58,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        matmul29_out1: Tensor<B, 4>,
        add10_out1: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let transpose19_out1 = matmul29_out1.permute([0, 2, 1, 3]);
        let shape18_out1: [i64; 4] = {
            let axes = &transpose19_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather15_out1 = shape18_out1[0] as i64;
        let gather16_out1 = shape18_out1[1] as i64;
        let unsqueeze26_out1 = [gather15_out1 as i64];
        let unsqueeze27_out1 = [gather16_out1 as i64];
        let constant401_out1: [i64; 1] = [384i64];
        let concat24_out1: [i64; 3usize] = [
            &unsqueeze26_out1[..],
            &unsqueeze27_out1[..],
            &constant401_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape20_out1 = transpose19_out1.reshape(concat24_out1);
        let linear22_out1 = self.linear22.forward(reshape20_out1);
        let constant46_out1 = self.constant46.val();
        let mul22_out1 = linear22_out1
            .mul((constant46_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add11_out1 = mul22_out1.add(add10_out1);
        let layernormalization8_out1 = {
            let dtype = add11_out1.clone().dtype();
            self.layernormalization8
                .forward(add11_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear23_out1 = self.linear23.forward(layernormalization8_out1);
        let constant402_out1 = self.constant402.val();
        let div6_out1 = linear23_out1
            .clone()
            .div((constant402_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf4_out1 = div6_out1.erf();
        let constant403_out1 = self.constant403.val();
        let add12_out1 = erf4_out1
            .add((constant403_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul23_out1 = linear23_out1.mul(add12_out1);
        let constant404_out1 = self.constant404.val();
        let mul24_out1 = mul23_out1
            .mul((constant404_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear24_out1 = self.linear24.forward(mul24_out1);
        let constant51_out1 = self.constant51.val();
        let mul25_out1 = linear24_out1
            .mul((constant51_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add13_out1 = mul25_out1.add(add11_out1);
        let layernormalization9_out1 = {
            let dtype = add13_out1.clone().dtype();
            self.layernormalization9
                .forward(add13_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape20_out1: [i64; 3] = {
            let axes = &layernormalization9_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather17_out1 = shape20_out1[0] as i64;
        let linear25_out1 = self.linear25.forward(layernormalization9_out1.clone());
        let unsqueeze28_out1 = [gather17_out1 as i64];
        let constant409_out1: [i64; 1] = [64i64];
        let constant407_out1: [i64; 1] = [-1i64];
        let constant408_out1: [i64; 1] = [6i64];
        let concat25_out1: [i64; 4usize] = [
            &unsqueeze28_out1[..],
            &constant407_out1[..],
            &constant408_out1[..],
            &constant409_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze29_out1 = [gather17_out1 as i64];
        let constant413_out1: [i64; 1] = [64i64];
        let constant411_out1: [i64; 1] = [-1i64];
        let constant412_out1: [i64; 1] = [6i64];
        let concat26_out1: [i64; 4usize] = [
            &unsqueeze29_out1[..],
            &constant411_out1[..],
            &constant412_out1[..],
            &constant413_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze30_out1 = [gather17_out1 as i64];
        let constant417_out1: [i64; 1] = [64i64];
        let constant415_out1: [i64; 1] = [-1i64];
        let constant416_out1: [i64; 1] = [6i64];
        let concat27_out1: [i64; 4usize] = [
            &unsqueeze30_out1[..],
            &constant415_out1[..],
            &constant416_out1[..],
            &constant417_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape21_out1 = linear25_out1.reshape(concat25_out1);
        let linear26_out1 = self.linear26.forward(layernormalization9_out1.clone());
        let reshape22_out1 = linear26_out1.reshape(concat26_out1);
        let transpose20_out1 = reshape22_out1.permute([0, 2, 1, 3]);
        let linear27_out1 = self.linear27.forward(layernormalization9_out1);
        let reshape23_out1 = linear27_out1.reshape(concat27_out1);
        let transpose21_out1 = reshape23_out1.permute([0, 2, 1, 3]);
        let transpose22_out1 = reshape21_out1.permute([0, 2, 3, 1]);
        let constant418_out1 = self.constant418.val();
        let mul26_out1 = transpose21_out1
            .mul((constant418_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant419_out1 = self.constant419.val();
        let mul27_out1 = transpose22_out1
            .mul((constant419_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul36_out1 = mul26_out1.matmul(mul27_out1);
        let softmax5_out1 = burn::tensor::activation::softmax(matmul36_out1, 3);
        let matmul37_out1 = softmax5_out1.matmul(transpose20_out1);
        let transpose23_out1 = matmul37_out1.permute([0, 2, 1, 3]);
        let shape21_out1: [i64; 4] = {
            let axes = &transpose23_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather18_out1 = shape21_out1[0] as i64;
        let gather19_out1 = shape21_out1[1] as i64;
        let unsqueeze31_out1 = [gather18_out1 as i64];
        let unsqueeze32_out1 = [gather19_out1 as i64];
        let constant424_out1: [i64; 1] = [384i64];
        let concat28_out1: [i64; 3usize] = [
            &unsqueeze31_out1[..],
            &unsqueeze32_out1[..],
            &constant424_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape24_out1 = transpose23_out1.reshape(concat28_out1);
        let linear28_out1 = self.linear28.forward(reshape24_out1);
        let constant58_out1 = self.constant58.val();
        let mul28_out1 = linear28_out1
            .mul((constant58_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add14_out1 = mul28_out1.add(add13_out1);
        add14_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule5<B: Backend> {
    layernormalization10: LayerNorm<B>,
    linear29: Linear<B>,
    constant425: burn::module::Param<Tensor<B, 1>>,
    constant426: burn::module::Param<Tensor<B, 1>>,
    constant427: burn::module::Param<Tensor<B, 1>>,
    linear30: Linear<B>,
    constant63: burn::module::Param<Tensor<B, 1>>,
    layernormalization11: LayerNorm<B>,
    linear31: Linear<B>,
    linear32: Linear<B>,
    linear33: Linear<B>,
    constant441: burn::module::Param<Tensor<B, 1>>,
    constant442: burn::module::Param<Tensor<B, 1>>,
    linear34: Linear<B>,
    constant70: burn::module::Param<Tensor<B, 1>>,
    layernormalization12: LayerNorm<B>,
    linear35: Linear<B>,
    constant448: burn::module::Param<Tensor<B, 1>>,
    constant449: burn::module::Param<Tensor<B, 1>>,
    constant450: burn::module::Param<Tensor<B, 1>>,
    linear36: Linear<B>,
    constant75: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule5<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization10 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear29 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant425: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant426: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant427: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear30 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant63: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization11 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear31 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear32 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear33 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant441: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant442: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear34 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant70: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization12 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear35 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant448: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant449: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant450: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear36 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant75: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            layernormalization10,
            linear29,
            constant425,
            constant426,
            constant427,
            linear30,
            constant63,
            layernormalization11,
            linear31,
            linear32,
            linear33,
            constant441,
            constant442,
            linear34,
            constant70,
            layernormalization12,
            linear35,
            constant448,
            constant449,
            constant450,
            linear36,
            constant75,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add14_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization10_out1 = {
            let dtype = add14_out1.clone().dtype();
            self.layernormalization10
                .forward(add14_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear29_out1 = self.linear29.forward(layernormalization10_out1);
        let constant425_out1 = self.constant425.val();
        let div7_out1 = linear29_out1
            .clone()
            .div((constant425_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf5_out1 = div7_out1.erf();
        let constant426_out1 = self.constant426.val();
        let add15_out1 = erf5_out1
            .add((constant426_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul29_out1 = linear29_out1.mul(add15_out1);
        let constant427_out1 = self.constant427.val();
        let mul30_out1 = mul29_out1
            .mul((constant427_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear30_out1 = self.linear30.forward(mul30_out1);
        let constant63_out1 = self.constant63.val();
        let mul31_out1 = linear30_out1
            .mul((constant63_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add16_out1 = mul31_out1.add(add14_out1);
        let layernormalization11_out1 = {
            let dtype = add16_out1.clone().dtype();
            self.layernormalization11
                .forward(add16_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape23_out1: [i64; 3] = {
            let axes = &layernormalization11_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather20_out1 = shape23_out1[0] as i64;
        let linear31_out1 = self.linear31.forward(layernormalization11_out1.clone());
        let unsqueeze33_out1 = [gather20_out1 as i64];
        let constant432_out1: [i64; 1] = [64i64];
        let constant430_out1: [i64; 1] = [-1i64];
        let constant431_out1: [i64; 1] = [6i64];
        let concat29_out1: [i64; 4usize] = [
            &unsqueeze33_out1[..],
            &constant430_out1[..],
            &constant431_out1[..],
            &constant432_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze34_out1 = [gather20_out1 as i64];
        let constant436_out1: [i64; 1] = [64i64];
        let constant434_out1: [i64; 1] = [-1i64];
        let constant435_out1: [i64; 1] = [6i64];
        let concat30_out1: [i64; 4usize] = [
            &unsqueeze34_out1[..],
            &constant434_out1[..],
            &constant435_out1[..],
            &constant436_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze35_out1 = [gather20_out1 as i64];
        let constant440_out1: [i64; 1] = [64i64];
        let constant438_out1: [i64; 1] = [-1i64];
        let constant439_out1: [i64; 1] = [6i64];
        let concat31_out1: [i64; 4usize] = [
            &unsqueeze35_out1[..],
            &constant438_out1[..],
            &constant439_out1[..],
            &constant440_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape25_out1 = linear31_out1.reshape(concat29_out1);
        let linear32_out1 = self.linear32.forward(layernormalization11_out1.clone());
        let reshape26_out1 = linear32_out1.reshape(concat30_out1);
        let transpose24_out1 = reshape26_out1.permute([0, 2, 1, 3]);
        let linear33_out1 = self.linear33.forward(layernormalization11_out1);
        let reshape27_out1 = linear33_out1.reshape(concat31_out1);
        let transpose25_out1 = reshape27_out1.permute([0, 2, 1, 3]);
        let transpose26_out1 = reshape25_out1.permute([0, 2, 3, 1]);
        let constant441_out1 = self.constant441.val();
        let mul32_out1 = transpose25_out1
            .mul((constant441_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant442_out1 = self.constant442.val();
        let mul33_out1 = transpose26_out1
            .mul((constant442_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul44_out1 = mul32_out1.matmul(mul33_out1);
        let softmax6_out1 = burn::tensor::activation::softmax(matmul44_out1, 3);
        let matmul45_out1 = softmax6_out1.matmul(transpose24_out1);
        let transpose27_out1 = matmul45_out1.permute([0, 2, 1, 3]);
        let shape24_out1: [i64; 4] = {
            let axes = &transpose27_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather21_out1 = shape24_out1[0] as i64;
        let gather22_out1 = shape24_out1[1] as i64;
        let unsqueeze36_out1 = [gather21_out1 as i64];
        let unsqueeze37_out1 = [gather22_out1 as i64];
        let constant447_out1: [i64; 1] = [384i64];
        let concat32_out1: [i64; 3usize] = [
            &unsqueeze36_out1[..],
            &unsqueeze37_out1[..],
            &constant447_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape28_out1 = transpose27_out1.reshape(concat32_out1);
        let linear34_out1 = self.linear34.forward(reshape28_out1);
        let constant70_out1 = self.constant70.val();
        let mul34_out1 = linear34_out1
            .mul((constant70_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add17_out1 = mul34_out1.add(add16_out1);
        let layernormalization12_out1 = {
            let dtype = add17_out1.clone().dtype();
            self.layernormalization12
                .forward(add17_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear35_out1 = self.linear35.forward(layernormalization12_out1);
        let constant448_out1 = self.constant448.val();
        let div8_out1 = linear35_out1
            .clone()
            .div((constant448_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf6_out1 = div8_out1.erf();
        let constant449_out1 = self.constant449.val();
        let add18_out1 = erf6_out1
            .add((constant449_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul35_out1 = linear35_out1.mul(add18_out1);
        let constant450_out1 = self.constant450.val();
        let mul36_out1 = mul35_out1
            .mul((constant450_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear36_out1 = self.linear36.forward(mul36_out1);
        let constant75_out1 = self.constant75.val();
        let mul37_out1 = linear36_out1
            .mul((constant75_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add19_out1 = mul37_out1.add(add17_out1);
        add19_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule6<B: Backend> {
    layernormalization13: LayerNorm<B>,
    linear37: Linear<B>,
    linear38: Linear<B>,
    linear39: Linear<B>,
    constant464: burn::module::Param<Tensor<B, 1>>,
    constant465: burn::module::Param<Tensor<B, 1>>,
    linear40: Linear<B>,
    constant82: burn::module::Param<Tensor<B, 1>>,
    layernormalization14: LayerNorm<B>,
    linear41: Linear<B>,
    constant471: burn::module::Param<Tensor<B, 1>>,
    constant472: burn::module::Param<Tensor<B, 1>>,
    constant473: burn::module::Param<Tensor<B, 1>>,
    linear42: Linear<B>,
    constant87: burn::module::Param<Tensor<B, 1>>,
    layernormalization15: LayerNorm<B>,
    linear43: Linear<B>,
    linear44: Linear<B>,
    linear45: Linear<B>,
    constant487: burn::module::Param<Tensor<B, 1>>,
    constant488: burn::module::Param<Tensor<B, 1>>,
    linear46: Linear<B>,
    constant94: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule6<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization13 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear37 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear38 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear39 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant464: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant465: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear40 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant82: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization14 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear41 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant471: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant472: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant473: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear42 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant87: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization15 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear43 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear44 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear45 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant487: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant488: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear46 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant94: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            layernormalization13,
            linear37,
            linear38,
            linear39,
            constant464,
            constant465,
            linear40,
            constant82,
            layernormalization14,
            linear41,
            constant471,
            constant472,
            constant473,
            linear42,
            constant87,
            layernormalization15,
            linear43,
            linear44,
            linear45,
            constant487,
            constant488,
            linear46,
            constant94,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add19_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization13_out1 = {
            let dtype = add19_out1.clone().dtype();
            self.layernormalization13
                .forward(add19_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape26_out1: [i64; 3] = {
            let axes = &layernormalization13_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather23_out1 = shape26_out1[0] as i64;
        let linear37_out1 = self.linear37.forward(layernormalization13_out1.clone());
        let unsqueeze38_out1 = [gather23_out1 as i64];
        let constant455_out1: [i64; 1] = [64i64];
        let constant453_out1: [i64; 1] = [-1i64];
        let constant454_out1: [i64; 1] = [6i64];
        let concat33_out1: [i64; 4usize] = [
            &unsqueeze38_out1[..],
            &constant453_out1[..],
            &constant454_out1[..],
            &constant455_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze39_out1 = [gather23_out1 as i64];
        let constant459_out1: [i64; 1] = [64i64];
        let constant457_out1: [i64; 1] = [-1i64];
        let constant458_out1: [i64; 1] = [6i64];
        let concat34_out1: [i64; 4usize] = [
            &unsqueeze39_out1[..],
            &constant457_out1[..],
            &constant458_out1[..],
            &constant459_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze40_out1 = [gather23_out1 as i64];
        let constant463_out1: [i64; 1] = [64i64];
        let constant461_out1: [i64; 1] = [-1i64];
        let constant462_out1: [i64; 1] = [6i64];
        let concat35_out1: [i64; 4usize] = [
            &unsqueeze40_out1[..],
            &constant461_out1[..],
            &constant462_out1[..],
            &constant463_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape29_out1 = linear37_out1.reshape(concat33_out1);
        let linear38_out1 = self.linear38.forward(layernormalization13_out1.clone());
        let reshape30_out1 = linear38_out1.reshape(concat34_out1);
        let transpose28_out1 = reshape30_out1.permute([0, 2, 1, 3]);
        let linear39_out1 = self.linear39.forward(layernormalization13_out1);
        let reshape31_out1 = linear39_out1.reshape(concat35_out1);
        let transpose29_out1 = reshape31_out1.permute([0, 2, 1, 3]);
        let transpose30_out1 = reshape29_out1.permute([0, 2, 3, 1]);
        let constant464_out1 = self.constant464.val();
        let mul38_out1 = transpose29_out1
            .mul((constant464_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant465_out1 = self.constant465.val();
        let mul39_out1 = transpose30_out1
            .mul((constant465_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul52_out1 = mul38_out1.matmul(mul39_out1);
        let softmax7_out1 = burn::tensor::activation::softmax(matmul52_out1, 3);
        let matmul53_out1 = softmax7_out1.matmul(transpose28_out1);
        let transpose31_out1 = matmul53_out1.permute([0, 2, 1, 3]);
        let shape27_out1: [i64; 4] = {
            let axes = &transpose31_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather24_out1 = shape27_out1[0] as i64;
        let gather25_out1 = shape27_out1[1] as i64;
        let unsqueeze41_out1 = [gather24_out1 as i64];
        let unsqueeze42_out1 = [gather25_out1 as i64];
        let constant470_out1: [i64; 1] = [384i64];
        let concat36_out1: [i64; 3usize] = [
            &unsqueeze41_out1[..],
            &unsqueeze42_out1[..],
            &constant470_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape32_out1 = transpose31_out1.reshape(concat36_out1);
        let linear40_out1 = self.linear40.forward(reshape32_out1);
        let constant82_out1 = self.constant82.val();
        let mul40_out1 = linear40_out1
            .mul((constant82_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add20_out1 = mul40_out1.add(add19_out1);
        let layernormalization14_out1 = {
            let dtype = add20_out1.clone().dtype();
            self.layernormalization14
                .forward(add20_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear41_out1 = self.linear41.forward(layernormalization14_out1);
        let constant471_out1 = self.constant471.val();
        let div9_out1 = linear41_out1
            .clone()
            .div((constant471_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf7_out1 = div9_out1.erf();
        let constant472_out1 = self.constant472.val();
        let add21_out1 = erf7_out1
            .add((constant472_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul41_out1 = linear41_out1.mul(add21_out1);
        let constant473_out1 = self.constant473.val();
        let mul42_out1 = mul41_out1
            .mul((constant473_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear42_out1 = self.linear42.forward(mul42_out1);
        let constant87_out1 = self.constant87.val();
        let mul43_out1 = linear42_out1
            .mul((constant87_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add22_out1 = mul43_out1.add(add20_out1);
        let layernormalization15_out1 = {
            let dtype = add22_out1.clone().dtype();
            self.layernormalization15
                .forward(add22_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape29_out1: [i64; 3] = {
            let axes = &layernormalization15_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather26_out1 = shape29_out1[0] as i64;
        let linear43_out1 = self.linear43.forward(layernormalization15_out1.clone());
        let unsqueeze43_out1 = [gather26_out1 as i64];
        let constant478_out1: [i64; 1] = [64i64];
        let constant476_out1: [i64; 1] = [-1i64];
        let constant477_out1: [i64; 1] = [6i64];
        let concat37_out1: [i64; 4usize] = [
            &unsqueeze43_out1[..],
            &constant476_out1[..],
            &constant477_out1[..],
            &constant478_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze44_out1 = [gather26_out1 as i64];
        let constant482_out1: [i64; 1] = [64i64];
        let constant480_out1: [i64; 1] = [-1i64];
        let constant481_out1: [i64; 1] = [6i64];
        let concat38_out1: [i64; 4usize] = [
            &unsqueeze44_out1[..],
            &constant480_out1[..],
            &constant481_out1[..],
            &constant482_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze45_out1 = [gather26_out1 as i64];
        let constant486_out1: [i64; 1] = [64i64];
        let constant484_out1: [i64; 1] = [-1i64];
        let constant485_out1: [i64; 1] = [6i64];
        let concat39_out1: [i64; 4usize] = [
            &unsqueeze45_out1[..],
            &constant484_out1[..],
            &constant485_out1[..],
            &constant486_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape33_out1 = linear43_out1.reshape(concat37_out1);
        let linear44_out1 = self.linear44.forward(layernormalization15_out1.clone());
        let reshape34_out1 = linear44_out1.reshape(concat38_out1);
        let transpose32_out1 = reshape34_out1.permute([0, 2, 1, 3]);
        let linear45_out1 = self.linear45.forward(layernormalization15_out1);
        let reshape35_out1 = linear45_out1.reshape(concat39_out1);
        let transpose33_out1 = reshape35_out1.permute([0, 2, 1, 3]);
        let transpose34_out1 = reshape33_out1.permute([0, 2, 3, 1]);
        let constant487_out1 = self.constant487.val();
        let mul44_out1 = transpose33_out1
            .mul((constant487_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant488_out1 = self.constant488.val();
        let mul45_out1 = transpose34_out1
            .mul((constant488_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul60_out1 = mul44_out1.matmul(mul45_out1);
        let softmax8_out1 = burn::tensor::activation::softmax(matmul60_out1, 3);
        let matmul61_out1 = softmax8_out1.matmul(transpose32_out1);
        let transpose35_out1 = matmul61_out1.permute([0, 2, 1, 3]);
        let shape30_out1: [i64; 4] = {
            let axes = &transpose35_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather27_out1 = shape30_out1[0] as i64;
        let gather28_out1 = shape30_out1[1] as i64;
        let unsqueeze46_out1 = [gather27_out1 as i64];
        let unsqueeze47_out1 = [gather28_out1 as i64];
        let constant493_out1: [i64; 1] = [384i64];
        let concat40_out1: [i64; 3usize] = [
            &unsqueeze46_out1[..],
            &unsqueeze47_out1[..],
            &constant493_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape36_out1 = transpose35_out1.reshape(concat40_out1);
        let linear46_out1 = self.linear46.forward(reshape36_out1);
        let constant94_out1 = self.constant94.val();
        let mul46_out1 = linear46_out1
            .mul((constant94_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add23_out1 = mul46_out1.add(add22_out1);
        add23_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule7<B: Backend> {
    layernormalization16: LayerNorm<B>,
    linear47: Linear<B>,
    constant494: burn::module::Param<Tensor<B, 1>>,
    constant495: burn::module::Param<Tensor<B, 1>>,
    constant496: burn::module::Param<Tensor<B, 1>>,
    linear48: Linear<B>,
    constant99: burn::module::Param<Tensor<B, 1>>,
    layernormalization17: LayerNorm<B>,
    linear49: Linear<B>,
    linear50: Linear<B>,
    linear51: Linear<B>,
    constant510: burn::module::Param<Tensor<B, 1>>,
    constant511: burn::module::Param<Tensor<B, 1>>,
    linear52: Linear<B>,
    constant106: burn::module::Param<Tensor<B, 1>>,
    layernormalization18: LayerNorm<B>,
    linear53: Linear<B>,
    constant517: burn::module::Param<Tensor<B, 1>>,
    constant518: burn::module::Param<Tensor<B, 1>>,
    constant519: burn::module::Param<Tensor<B, 1>>,
    linear54: Linear<B>,
    constant111: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule7<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization16 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear47 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant494: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant495: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant496: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear48 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant99: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization17 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear49 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear50 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear51 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant510: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant511: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear52 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant106: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization18 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear53 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant517: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant518: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant519: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear54 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant111: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        Self {
            layernormalization16,
            linear47,
            constant494,
            constant495,
            constant496,
            linear48,
            constant99,
            layernormalization17,
            linear49,
            linear50,
            linear51,
            constant510,
            constant511,
            linear52,
            constant106,
            layernormalization18,
            linear53,
            constant517,
            constant518,
            constant519,
            linear54,
            constant111,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add23_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization16_out1 = {
            let dtype = add23_out1.clone().dtype();
            self.layernormalization16
                .forward(add23_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear47_out1 = self.linear47.forward(layernormalization16_out1);
        let constant494_out1 = self.constant494.val();
        let div10_out1 = linear47_out1
            .clone()
            .div((constant494_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf8_out1 = div10_out1.erf();
        let constant495_out1 = self.constant495.val();
        let add24_out1 = erf8_out1
            .add((constant495_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul47_out1 = linear47_out1.mul(add24_out1);
        let constant496_out1 = self.constant496.val();
        let mul48_out1 = mul47_out1
            .mul((constant496_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear48_out1 = self.linear48.forward(mul48_out1);
        let constant99_out1 = self.constant99.val();
        let mul49_out1 = linear48_out1
            .mul((constant99_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add25_out1 = mul49_out1.add(add23_out1);
        let layernormalization17_out1 = {
            let dtype = add25_out1.clone().dtype();
            self.layernormalization17
                .forward(add25_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape32_out1: [i64; 3] = {
            let axes = &layernormalization17_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather29_out1 = shape32_out1[0] as i64;
        let linear49_out1 = self.linear49.forward(layernormalization17_out1.clone());
        let unsqueeze48_out1 = [gather29_out1 as i64];
        let constant501_out1: [i64; 1] = [64i64];
        let constant499_out1: [i64; 1] = [-1i64];
        let constant500_out1: [i64; 1] = [6i64];
        let concat41_out1: [i64; 4usize] = [
            &unsqueeze48_out1[..],
            &constant499_out1[..],
            &constant500_out1[..],
            &constant501_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze49_out1 = [gather29_out1 as i64];
        let constant505_out1: [i64; 1] = [64i64];
        let constant503_out1: [i64; 1] = [-1i64];
        let constant504_out1: [i64; 1] = [6i64];
        let concat42_out1: [i64; 4usize] = [
            &unsqueeze49_out1[..],
            &constant503_out1[..],
            &constant504_out1[..],
            &constant505_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze50_out1 = [gather29_out1 as i64];
        let constant509_out1: [i64; 1] = [64i64];
        let constant507_out1: [i64; 1] = [-1i64];
        let constant508_out1: [i64; 1] = [6i64];
        let concat43_out1: [i64; 4usize] = [
            &unsqueeze50_out1[..],
            &constant507_out1[..],
            &constant508_out1[..],
            &constant509_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape37_out1 = linear49_out1.reshape(concat41_out1);
        let linear50_out1 = self.linear50.forward(layernormalization17_out1.clone());
        let reshape38_out1 = linear50_out1.reshape(concat42_out1);
        let transpose36_out1 = reshape38_out1.permute([0, 2, 1, 3]);
        let linear51_out1 = self.linear51.forward(layernormalization17_out1);
        let reshape39_out1 = linear51_out1.reshape(concat43_out1);
        let transpose37_out1 = reshape39_out1.permute([0, 2, 1, 3]);
        let transpose38_out1 = reshape37_out1.permute([0, 2, 3, 1]);
        let constant510_out1 = self.constant510.val();
        let mul50_out1 = transpose37_out1
            .mul((constant510_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant511_out1 = self.constant511.val();
        let mul51_out1 = transpose38_out1
            .mul((constant511_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul68_out1 = mul50_out1.matmul(mul51_out1);
        let softmax9_out1 = burn::tensor::activation::softmax(matmul68_out1, 3);
        let matmul69_out1 = softmax9_out1.matmul(transpose36_out1);
        let transpose39_out1 = matmul69_out1.permute([0, 2, 1, 3]);
        let shape33_out1: [i64; 4] = {
            let axes = &transpose39_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather30_out1 = shape33_out1[0] as i64;
        let gather31_out1 = shape33_out1[1] as i64;
        let unsqueeze51_out1 = [gather30_out1 as i64];
        let unsqueeze52_out1 = [gather31_out1 as i64];
        let constant516_out1: [i64; 1] = [384i64];
        let concat44_out1: [i64; 3usize] = [
            &unsqueeze51_out1[..],
            &unsqueeze52_out1[..],
            &constant516_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape40_out1 = transpose39_out1.reshape(concat44_out1);
        let linear52_out1 = self.linear52.forward(reshape40_out1);
        let constant106_out1 = self.constant106.val();
        let mul52_out1 = linear52_out1
            .mul((constant106_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add26_out1 = mul52_out1.add(add25_out1);
        let layernormalization18_out1 = {
            let dtype = add26_out1.clone().dtype();
            self.layernormalization18
                .forward(add26_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear53_out1 = self.linear53.forward(layernormalization18_out1);
        let constant517_out1 = self.constant517.val();
        let div11_out1 = linear53_out1
            .clone()
            .div((constant517_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf9_out1 = div11_out1.erf();
        let constant518_out1 = self.constant518.val();
        let add27_out1 = erf9_out1
            .add((constant518_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul53_out1 = linear53_out1.mul(add27_out1);
        let constant519_out1 = self.constant519.val();
        let mul54_out1 = mul53_out1
            .mul((constant519_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear54_out1 = self.linear54.forward(mul54_out1);
        let constant111_out1 = self.constant111.val();
        let mul55_out1 = linear54_out1
            .mul((constant111_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add28_out1 = mul55_out1.add(add26_out1);
        add28_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule8<B: Backend> {
    layernormalization19: LayerNorm<B>,
    linear55: Linear<B>,
    linear56: Linear<B>,
    linear57: Linear<B>,
    constant533: burn::module::Param<Tensor<B, 1>>,
    constant534: burn::module::Param<Tensor<B, 1>>,
    linear58: Linear<B>,
    constant118: burn::module::Param<Tensor<B, 1>>,
    layernormalization20: LayerNorm<B>,
    linear59: Linear<B>,
    constant540: burn::module::Param<Tensor<B, 1>>,
    constant541: burn::module::Param<Tensor<B, 1>>,
    constant542: burn::module::Param<Tensor<B, 1>>,
    linear60: Linear<B>,
    constant123: burn::module::Param<Tensor<B, 1>>,
    layernormalization21: LayerNorm<B>,
    linear61: Linear<B>,
    linear62: Linear<B>,
    linear63: Linear<B>,
    constant556: burn::module::Param<Tensor<B, 1>>,
    constant557: burn::module::Param<Tensor<B, 1>>,
    linear64: Linear<B>,
    constant130: burn::module::Param<Tensor<B, 1>>,
    layernormalization22: LayerNorm<B>,
    linear65: Linear<B>,
    constant563: burn::module::Param<Tensor<B, 1>>,
    constant564: burn::module::Param<Tensor<B, 1>>,
    constant565: burn::module::Param<Tensor<B, 1>>,
    linear66: Linear<B>,
    constant135: burn::module::Param<Tensor<B, 1>>,
    layernormalization23: LayerNorm<B>,
    linear67: Linear<B>,
    linear68: Linear<B>,
    linear69: Linear<B>,
    constant579: burn::module::Param<Tensor<B, 1>>,
    constant580: burn::module::Param<Tensor<B, 1>>,
    linear70: Linear<B>,
    constant142: burn::module::Param<Tensor<B, 1>>,
    layernormalization24: LayerNorm<B>,
    linear71: Linear<B>,
    constant586: burn::module::Param<Tensor<B, 1>>,
    constant587: burn::module::Param<Tensor<B, 1>>,
    constant588: burn::module::Param<Tensor<B, 1>>,
    linear72: Linear<B>,
    constant147: burn::module::Param<Tensor<B, 1>>,
    layernormalization25: LayerNorm<B>,
    layernormalization26: LayerNorm<B>,
    layernormalization27: LayerNorm<B>,
    layernormalization28: LayerNorm<B>,
    conv2d2: Conv2d<B>,
    convtranspose2d1: ConvTranspose2d<B>,
    conv2d3: Conv2d<B>,
    convtranspose2d2: ConvTranspose2d<B>,
    conv2d4: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule8<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization19 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear55 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear56 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear57 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant533: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant534: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear58 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant118: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization20 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear59 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant540: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant541: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant542: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear60 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant123: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization21 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear61 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear62 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear63 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant556: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant557: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear64 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant130: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization22 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear65 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant563: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant564: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant565: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear66 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant135: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization23 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear67 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear68 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear69 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant579: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant580: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.3535533845424652f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear70 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant142: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization24 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear71 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant586: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4142135381698608f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant587: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant588: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear72 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant147: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization25 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization26 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization27 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization28 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d2 = Conv2dConfig::new([384, 48], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let convtranspose2d1 = ConvTranspose2dConfig::new([48, 48], [4, 4])
            .with_stride([4, 4])
            .with_padding([0, 0])
            .with_padding_out([0, 0])
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d3 = Conv2dConfig::new([384, 96], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let convtranspose2d2 = ConvTranspose2dConfig::new([96, 96], [2, 2])
            .with_stride([2, 2])
            .with_padding([0, 0])
            .with_padding_out([0, 0])
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d4 = Conv2dConfig::new([384, 192], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            layernormalization19,
            linear55,
            linear56,
            linear57,
            constant533,
            constant534,
            linear58,
            constant118,
            layernormalization20,
            linear59,
            constant540,
            constant541,
            constant542,
            linear60,
            constant123,
            layernormalization21,
            linear61,
            linear62,
            linear63,
            constant556,
            constant557,
            linear64,
            constant130,
            layernormalization22,
            linear65,
            constant563,
            constant564,
            constant565,
            linear66,
            constant135,
            layernormalization23,
            linear67,
            linear68,
            linear69,
            constant579,
            constant580,
            linear70,
            constant142,
            layernormalization24,
            linear71,
            constant586,
            constant587,
            constant588,
            linear72,
            constant147,
            layernormalization25,
            layernormalization26,
            layernormalization27,
            layernormalization28,
            conv2d2,
            convtranspose2d1,
            conv2d3,
            convtranspose2d2,
            conv2d4,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add28_out1: Tensor<B, 3>,
        add10_out1: Tensor<B, 3>,
        add19_out1: Tensor<B, 3>,
        shape1_out1: [i64; 4],
    ) -> (Tensor<B, 4>, Tensor<B, 4>, Tensor<B, 4>, Tensor<B, 4>) {
        let layernormalization19_out1 = {
            let dtype = add28_out1.clone().dtype();
            self.layernormalization19
                .forward(add28_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape35_out1: [i64; 3] = {
            let axes = &layernormalization19_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather32_out1 = shape35_out1[0] as i64;
        let linear55_out1 = self.linear55.forward(layernormalization19_out1.clone());
        let unsqueeze53_out1 = [gather32_out1 as i64];
        let constant524_out1: [i64; 1] = [64i64];
        let constant522_out1: [i64; 1] = [-1i64];
        let constant523_out1: [i64; 1] = [6i64];
        let concat45_out1: [i64; 4usize] = [
            &unsqueeze53_out1[..],
            &constant522_out1[..],
            &constant523_out1[..],
            &constant524_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze54_out1 = [gather32_out1 as i64];
        let constant528_out1: [i64; 1] = [64i64];
        let constant526_out1: [i64; 1] = [-1i64];
        let constant527_out1: [i64; 1] = [6i64];
        let concat46_out1: [i64; 4usize] = [
            &unsqueeze54_out1[..],
            &constant526_out1[..],
            &constant527_out1[..],
            &constant528_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze55_out1 = [gather32_out1 as i64];
        let constant532_out1: [i64; 1] = [64i64];
        let constant530_out1: [i64; 1] = [-1i64];
        let constant531_out1: [i64; 1] = [6i64];
        let concat47_out1: [i64; 4usize] = [
            &unsqueeze55_out1[..],
            &constant530_out1[..],
            &constant531_out1[..],
            &constant532_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape41_out1 = linear55_out1.reshape(concat45_out1);
        let linear56_out1 = self.linear56.forward(layernormalization19_out1.clone());
        let reshape42_out1 = linear56_out1.reshape(concat46_out1);
        let transpose40_out1 = reshape42_out1.permute([0, 2, 1, 3]);
        let linear57_out1 = self.linear57.forward(layernormalization19_out1);
        let reshape43_out1 = linear57_out1.reshape(concat47_out1);
        let transpose41_out1 = reshape43_out1.permute([0, 2, 1, 3]);
        let transpose42_out1 = reshape41_out1.permute([0, 2, 3, 1]);
        let constant533_out1 = self.constant533.val();
        let mul56_out1 = transpose41_out1
            .mul((constant533_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant534_out1 = self.constant534.val();
        let mul57_out1 = transpose42_out1
            .mul((constant534_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul76_out1 = mul56_out1.matmul(mul57_out1);
        let softmax10_out1 = burn::tensor::activation::softmax(matmul76_out1, 3);
        let matmul77_out1 = softmax10_out1.matmul(transpose40_out1);
        let transpose43_out1 = matmul77_out1.permute([0, 2, 1, 3]);
        let shape36_out1: [i64; 4] = {
            let axes = &transpose43_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather33_out1 = shape36_out1[0] as i64;
        let gather34_out1 = shape36_out1[1] as i64;
        let unsqueeze56_out1 = [gather33_out1 as i64];
        let unsqueeze57_out1 = [gather34_out1 as i64];
        let constant539_out1: [i64; 1] = [384i64];
        let concat48_out1: [i64; 3usize] = [
            &unsqueeze56_out1[..],
            &unsqueeze57_out1[..],
            &constant539_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape44_out1 = transpose43_out1.reshape(concat48_out1);
        let linear58_out1 = self.linear58.forward(reshape44_out1);
        let constant118_out1 = self.constant118.val();
        let mul58_out1 = linear58_out1
            .mul((constant118_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add29_out1 = mul58_out1.add(add28_out1.clone());
        let layernormalization20_out1 = {
            let dtype = add29_out1.clone().dtype();
            self.layernormalization20
                .forward(add29_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear59_out1 = self.linear59.forward(layernormalization20_out1);
        let constant540_out1 = self.constant540.val();
        let div12_out1 = linear59_out1
            .clone()
            .div((constant540_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf10_out1 = div12_out1.erf();
        let constant541_out1 = self.constant541.val();
        let add30_out1 = erf10_out1
            .add((constant541_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul59_out1 = linear59_out1.mul(add30_out1);
        let constant542_out1 = self.constant542.val();
        let mul60_out1 = mul59_out1
            .mul((constant542_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear60_out1 = self.linear60.forward(mul60_out1);
        let constant123_out1 = self.constant123.val();
        let mul61_out1 = linear60_out1
            .mul((constant123_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add31_out1 = mul61_out1.add(add29_out1);
        let layernormalization21_out1 = {
            let dtype = add31_out1.clone().dtype();
            self.layernormalization21
                .forward(add31_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape38_out1: [i64; 3] = {
            let axes = &layernormalization21_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather35_out1 = shape38_out1[0] as i64;
        let linear61_out1 = self.linear61.forward(layernormalization21_out1.clone());
        let unsqueeze58_out1 = [gather35_out1 as i64];
        let constant547_out1: [i64; 1] = [64i64];
        let constant545_out1: [i64; 1] = [-1i64];
        let constant546_out1: [i64; 1] = [6i64];
        let concat49_out1: [i64; 4usize] = [
            &unsqueeze58_out1[..],
            &constant545_out1[..],
            &constant546_out1[..],
            &constant547_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze59_out1 = [gather35_out1 as i64];
        let constant551_out1: [i64; 1] = [64i64];
        let constant549_out1: [i64; 1] = [-1i64];
        let constant550_out1: [i64; 1] = [6i64];
        let concat50_out1: [i64; 4usize] = [
            &unsqueeze59_out1[..],
            &constant549_out1[..],
            &constant550_out1[..],
            &constant551_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze60_out1 = [gather35_out1 as i64];
        let constant555_out1: [i64; 1] = [64i64];
        let constant553_out1: [i64; 1] = [-1i64];
        let constant554_out1: [i64; 1] = [6i64];
        let concat51_out1: [i64; 4usize] = [
            &unsqueeze60_out1[..],
            &constant553_out1[..],
            &constant554_out1[..],
            &constant555_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape45_out1 = linear61_out1.reshape(concat49_out1);
        let linear62_out1 = self.linear62.forward(layernormalization21_out1.clone());
        let reshape46_out1 = linear62_out1.reshape(concat50_out1);
        let transpose44_out1 = reshape46_out1.permute([0, 2, 1, 3]);
        let linear63_out1 = self.linear63.forward(layernormalization21_out1);
        let reshape47_out1 = linear63_out1.reshape(concat51_out1);
        let transpose45_out1 = reshape47_out1.permute([0, 2, 1, 3]);
        let transpose46_out1 = reshape45_out1.permute([0, 2, 3, 1]);
        let constant556_out1 = self.constant556.val();
        let mul62_out1 = transpose45_out1
            .mul((constant556_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant557_out1 = self.constant557.val();
        let mul63_out1 = transpose46_out1
            .mul((constant557_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul84_out1 = mul62_out1.matmul(mul63_out1);
        let softmax11_out1 = burn::tensor::activation::softmax(matmul84_out1, 3);
        let matmul85_out1 = softmax11_out1.matmul(transpose44_out1);
        let transpose47_out1 = matmul85_out1.permute([0, 2, 1, 3]);
        let shape39_out1: [i64; 4] = {
            let axes = &transpose47_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather36_out1 = shape39_out1[0] as i64;
        let gather37_out1 = shape39_out1[1] as i64;
        let unsqueeze61_out1 = [gather36_out1 as i64];
        let unsqueeze62_out1 = [gather37_out1 as i64];
        let constant562_out1: [i64; 1] = [384i64];
        let concat52_out1: [i64; 3usize] = [
            &unsqueeze61_out1[..],
            &unsqueeze62_out1[..],
            &constant562_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape48_out1 = transpose47_out1.reshape(concat52_out1);
        let linear64_out1 = self.linear64.forward(reshape48_out1);
        let constant130_out1 = self.constant130.val();
        let mul64_out1 = linear64_out1
            .mul((constant130_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add32_out1 = mul64_out1.add(add31_out1);
        let layernormalization22_out1 = {
            let dtype = add32_out1.clone().dtype();
            self.layernormalization22
                .forward(add32_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear65_out1 = self.linear65.forward(layernormalization22_out1);
        let constant563_out1 = self.constant563.val();
        let div13_out1 = linear65_out1
            .clone()
            .div((constant563_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf11_out1 = div13_out1.erf();
        let constant564_out1 = self.constant564.val();
        let add33_out1 = erf11_out1
            .add((constant564_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul65_out1 = linear65_out1.mul(add33_out1);
        let constant565_out1 = self.constant565.val();
        let mul66_out1 = mul65_out1
            .mul((constant565_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear66_out1 = self.linear66.forward(mul66_out1);
        let constant135_out1 = self.constant135.val();
        let mul67_out1 = linear66_out1
            .mul((constant135_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add34_out1 = mul67_out1.add(add32_out1);
        let layernormalization23_out1 = {
            let dtype = add34_out1.clone().dtype();
            self.layernormalization23
                .forward(add34_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let shape41_out1: [i64; 3] = {
            let axes = &layernormalization23_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather38_out1 = shape41_out1[0] as i64;
        let linear67_out1 = self.linear67.forward(layernormalization23_out1.clone());
        let unsqueeze63_out1 = [gather38_out1 as i64];
        let constant570_out1: [i64; 1] = [64i64];
        let constant568_out1: [i64; 1] = [-1i64];
        let constant569_out1: [i64; 1] = [6i64];
        let concat53_out1: [i64; 4usize] = [
            &unsqueeze63_out1[..],
            &constant568_out1[..],
            &constant569_out1[..],
            &constant570_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze64_out1 = [gather38_out1 as i64];
        let constant574_out1: [i64; 1] = [64i64];
        let constant572_out1: [i64; 1] = [-1i64];
        let constant573_out1: [i64; 1] = [6i64];
        let concat54_out1: [i64; 4usize] = [
            &unsqueeze64_out1[..],
            &constant572_out1[..],
            &constant573_out1[..],
            &constant574_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze65_out1 = [gather38_out1 as i64];
        let constant578_out1: [i64; 1] = [64i64];
        let constant576_out1: [i64; 1] = [-1i64];
        let constant577_out1: [i64; 1] = [6i64];
        let concat55_out1: [i64; 4usize] = [
            &unsqueeze65_out1[..],
            &constant576_out1[..],
            &constant577_out1[..],
            &constant578_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape49_out1 = linear67_out1.reshape(concat53_out1);
        let linear68_out1 = self.linear68.forward(layernormalization23_out1.clone());
        let reshape50_out1 = linear68_out1.reshape(concat54_out1);
        let transpose48_out1 = reshape50_out1.permute([0, 2, 1, 3]);
        let linear69_out1 = self.linear69.forward(layernormalization23_out1);
        let reshape51_out1 = linear69_out1.reshape(concat55_out1);
        let transpose49_out1 = reshape51_out1.permute([0, 2, 1, 3]);
        let transpose50_out1 = reshape49_out1.permute([0, 2, 3, 1]);
        let constant579_out1 = self.constant579.val();
        let mul68_out1 = transpose49_out1
            .mul((constant579_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant580_out1 = self.constant580.val();
        let mul69_out1 = transpose50_out1
            .mul((constant580_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul92_out1 = mul68_out1.matmul(mul69_out1);
        let softmax12_out1 = burn::tensor::activation::softmax(matmul92_out1, 3);
        let matmul93_out1 = softmax12_out1.matmul(transpose48_out1);
        let transpose51_out1 = matmul93_out1.permute([0, 2, 1, 3]);
        let shape42_out1: [i64; 4] = {
            let axes = &transpose51_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather39_out1 = shape42_out1[0] as i64;
        let gather40_out1 = shape42_out1[1] as i64;
        let unsqueeze66_out1 = [gather39_out1 as i64];
        let unsqueeze67_out1 = [gather40_out1 as i64];
        let constant585_out1: [i64; 1] = [384i64];
        let concat56_out1: [i64; 3usize] = [
            &unsqueeze66_out1[..],
            &unsqueeze67_out1[..],
            &constant585_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape52_out1 = transpose51_out1.reshape(concat56_out1);
        let linear70_out1 = self.linear70.forward(reshape52_out1);
        let constant142_out1 = self.constant142.val();
        let mul70_out1 = linear70_out1
            .mul((constant142_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add35_out1 = mul70_out1.add(add34_out1);
        let layernormalization24_out1 = {
            let dtype = add35_out1.clone().dtype();
            self.layernormalization24
                .forward(add35_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear71_out1 = self.linear71.forward(layernormalization24_out1);
        let constant586_out1 = self.constant586.val();
        let div14_out1 = linear71_out1
            .clone()
            .div((constant586_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf12_out1 = div14_out1.erf();
        let constant587_out1 = self.constant587.val();
        let add36_out1 = erf12_out1
            .add((constant587_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul71_out1 = linear71_out1.mul(add36_out1);
        let constant588_out1 = self.constant588.val();
        let mul72_out1 = mul71_out1
            .mul((constant588_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear72_out1 = self.linear72.forward(mul72_out1);
        let constant147_out1 = self.constant147.val();
        let mul73_out1 = linear72_out1
            .mul((constant147_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add37_out1 = mul73_out1.add(add35_out1);
        let layernormalization25_out1 = {
            let dtype = add10_out1.dtype();
            self.layernormalization25
                .forward(add10_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let layernormalization26_out1 = {
            let dtype = add19_out1.dtype();
            self.layernormalization26
                .forward(add19_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let layernormalization27_out1 = {
            let dtype = add28_out1.dtype();
            self.layernormalization27
                .forward(add28_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let layernormalization28_out1 = {
            let dtype = add37_out1.dtype();
            self.layernormalization28
                .forward(add37_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let gather41_out1 = shape1_out1[2] as i64;
        let gather42_out1 = shape1_out1[3] as i64;
        let constant591_out1 = 14i64;
        let div15_out1 = gather41_out1 / constant591_out1;
        let constant592_out1 = 14i64;
        let div16_out1 = gather42_out1 / constant592_out1;
        let slice3_out1 = layernormalization25_out1.slice(s![.., 1.., ..]);
        let shape46_out1: [i64; 3] = {
            let axes = &slice3_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather43_out1 = shape46_out1[0] as i64;
        let gather44_out1 = shape46_out1[2] as i64;
        let unsqueeze68_out1 = [gather43_out1 as i64];
        let unsqueeze69_out1 = [div15_out1 as i64];
        let unsqueeze70_out1 = [div16_out1 as i64];
        let unsqueeze71_out1 = [gather44_out1 as i64];
        let concat57_out1: [i64; 4usize] = [
            &unsqueeze68_out1[..],
            &unsqueeze69_out1[..],
            &unsqueeze70_out1[..],
            &unsqueeze71_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape53_out1 = slice3_out1.reshape(concat57_out1);
        let transpose52_out1 = reshape53_out1.permute([0, 3, 1, 2]);
        let conv2d2_out1 = self.conv2d2.forward(transpose52_out1);
        let convtranspose2d1_out1 = self.convtranspose2d1.forward(conv2d2_out1);
        let slice4_out1 = layernormalization26_out1.slice(s![.., 1.., ..]);
        let shape48_out1: [i64; 3] = {
            let axes = &slice4_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather45_out1 = shape48_out1[0] as i64;
        let gather46_out1 = shape48_out1[2] as i64;
        let unsqueeze72_out1 = [gather45_out1 as i64];
        let unsqueeze73_out1 = [div15_out1 as i64];
        let unsqueeze74_out1 = [div16_out1 as i64];
        let unsqueeze75_out1 = [gather46_out1 as i64];
        let concat58_out1: [i64; 4usize] = [
            &unsqueeze72_out1[..],
            &unsqueeze73_out1[..],
            &unsqueeze74_out1[..],
            &unsqueeze75_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape54_out1 = slice4_out1.reshape(concat58_out1);
        let transpose53_out1 = reshape54_out1.permute([0, 3, 1, 2]);
        let conv2d3_out1 = self.conv2d3.forward(transpose53_out1);
        let convtranspose2d2_out1 = self.convtranspose2d2.forward(conv2d3_out1);
        let slice5_out1 = layernormalization27_out1.slice(s![.., 1.., ..]);
        let shape50_out1: [i64; 3] = {
            let axes = &slice5_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather47_out1 = shape50_out1[0] as i64;
        let gather48_out1 = shape50_out1[2] as i64;
        let unsqueeze76_out1 = [gather47_out1 as i64];
        let unsqueeze77_out1 = [div15_out1 as i64];
        let unsqueeze78_out1 = [div16_out1 as i64];
        let unsqueeze79_out1 = [gather48_out1 as i64];
        let concat59_out1: [i64; 4usize] = [
            &unsqueeze76_out1[..],
            &unsqueeze77_out1[..],
            &unsqueeze78_out1[..],
            &unsqueeze79_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape55_out1 = slice5_out1.reshape(concat59_out1);
        let transpose54_out1 = reshape55_out1.permute([0, 3, 1, 2]);
        let conv2d4_out1 = self.conv2d4.forward(transpose54_out1);
        let slice6_out1 = layernormalization28_out1.slice(s![.., 1.., ..]);
        let shape52_out1: [i64; 3] = {
            let axes = &slice6_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather49_out1 = shape52_out1[0] as i64;
        let gather50_out1 = shape52_out1[2] as i64;
        let unsqueeze80_out1 = [gather49_out1 as i64];
        let unsqueeze81_out1 = [div15_out1 as i64];
        let unsqueeze82_out1 = [div16_out1 as i64];
        let unsqueeze83_out1 = [gather50_out1 as i64];
        let concat60_out1: [i64; 4usize] = [
            &unsqueeze80_out1[..],
            &unsqueeze81_out1[..],
            &unsqueeze82_out1[..],
            &unsqueeze83_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape56_out1 = slice6_out1.reshape(concat60_out1);
        (reshape56_out1, convtranspose2d1_out1, convtranspose2d2_out1, conv2d4_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule9<B: Backend> {
    conv2d5: Conv2d<B>,
    conv2d6: Conv2d<B>,
    conv2d7: Conv2d<B>,
    conv2d8: Conv2d<B>,
    conv2d9: Conv2d<B>,
    conv2d10: Conv2d<B>,
    conv2d11: Conv2d<B>,
    conv2d12: Conv2d<B>,
    conv2d13: Conv2d<B>,
    conv2d14: Conv2d<B>,
    conv2d15: Conv2d<B>,
    conv2d16: Conv2d<B>,
    conv2d17: Conv2d<B>,
    conv2d18: Conv2d<B>,
    conv2d19: Conv2d<B>,
    conv2d20: Conv2d<B>,
    conv2d21: Conv2d<B>,
    conv2d22: Conv2d<B>,
    conv2d23: Conv2d<B>,
    conv2d24: Conv2d<B>,
    conv2d25: Conv2d<B>,
    conv2d26: Conv2d<B>,
    conv2d27: Conv2d<B>,
    resize5: burn::nn::interpolate::Interpolate2d,
    conv2d28: Conv2d<B>,
    conv2d29: Conv2d<B>,
    conv2d30: Conv2d<B>,
    conv2d31: Conv2d<B>,
    constant659: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule9<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d5 = Conv2dConfig::new([384, 384], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d6 = Conv2dConfig::new([384, 384], [3, 3])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d7 = Conv2dConfig::new([48, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d8 = Conv2dConfig::new([96, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d9 = Conv2dConfig::new([192, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d10 = Conv2dConfig::new([384, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d11 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d12 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d13 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d14 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d15 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d16 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d17 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d18 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d19 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d20 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d21 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d22 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d23 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d24 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d25 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d26 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d27 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let resize5 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(None)
            .with_scale_factor(Some([2.0, 2.0]))
            .with_mode(burn::nn::interpolate::InterpolateMode::Linear)
            .with_align_corners(true)
            .init();
        let conv2d28 = Conv2dConfig::new([64, 64], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d29 = Conv2dConfig::new([64, 32], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d30 = Conv2dConfig::new([32, 32], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d31 = Conv2dConfig::new([32, 1], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant659: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([20f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        Self {
            conv2d5,
            conv2d6,
            conv2d7,
            conv2d8,
            conv2d9,
            conv2d10,
            conv2d11,
            conv2d12,
            conv2d13,
            conv2d14,
            conv2d15,
            conv2d16,
            conv2d17,
            conv2d18,
            conv2d19,
            conv2d20,
            conv2d21,
            conv2d22,
            conv2d23,
            conv2d24,
            conv2d25,
            conv2d26,
            conv2d27,
            resize5,
            conv2d28,
            conv2d29,
            conv2d30,
            conv2d31,
            constant659,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        reshape56_out1: Tensor<B, 4>,
        convtranspose2d1_out1: Tensor<B, 4>,
        convtranspose2d2_out1: Tensor<B, 4>,
        conv2d4_out1: Tensor<B, 4>,
    ) -> Tensor<B, 3> {
        let transpose55_out1 = reshape56_out1.permute([0, 3, 1, 2]);
        let conv2d5_out1 = self.conv2d5.forward(transpose55_out1);
        let conv2d6_out1 = self.conv2d6.forward(conv2d5_out1);
        let conv2d7_out1 = self.conv2d7.forward(convtranspose2d1_out1);
        let conv2d8_out1 = self.conv2d8.forward(convtranspose2d2_out1);
        let conv2d9_out1 = self.conv2d9.forward(conv2d4_out1);
        let conv2d10_out1 = self.conv2d10.forward(conv2d6_out1);
        let shape54_out1: [i64; 4] = {
            let axes = &conv2d9_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather51_out1 = shape54_out1[2] as i64;
        let gather52_out1 = shape54_out1[3] as i64;
        let relu1_out1 = burn::tensor::activation::relu(conv2d10_out1.clone());
        let conv2d11_out1 = self.conv2d11.forward(relu1_out1);
        let relu2_out1 = burn::tensor::activation::relu(conv2d11_out1);
        let conv2d12_out1 = self.conv2d12.forward(relu2_out1);
        let add38_out1 = conv2d12_out1.add(conv2d10_out1);
        let unsqueeze84_out1 = [gather51_out1 as i64];
        let unsqueeze85_out1 = [gather52_out1 as i64];
        let concat61_out1: [i64; 2usize] = [&unsqueeze84_out1[..], &unsqueeze85_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let shape56_out1: [i64; 4] = {
            let axes = &add38_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice7_out1: [i64; 2] = shape56_out1[0..2].try_into().unwrap();
        let concat62_out1: [i64; 4usize] = [&slice7_out1[..], &concat61_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let resize2_out1 = {
            let target_height = concat62_out1[2] as usize;
            let target_width = concat62_out1[3] as usize;
            burn::tensor::module::interpolate(
                add38_out1,
                [target_height, target_width],
                burn::tensor::ops::InterpolateOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_align_corners(true),
            )
        };
        let conv2d13_out1 = self.conv2d13.forward(resize2_out1);
        let shape57_out1: [i64; 4] = {
            let axes = &conv2d8_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather53_out1 = shape57_out1[2] as i64;
        let gather54_out1 = shape57_out1[3] as i64;
        let relu3_out1 = burn::tensor::activation::relu(conv2d9_out1.clone());
        let conv2d14_out1 = self.conv2d14.forward(relu3_out1);
        let relu4_out1 = burn::tensor::activation::relu(conv2d14_out1);
        let conv2d15_out1 = self.conv2d15.forward(relu4_out1);
        let add39_out1 = conv2d15_out1.add(conv2d9_out1);
        let add40_out1 = conv2d13_out1.add(add39_out1);
        let relu5_out1 = burn::tensor::activation::relu(add40_out1.clone());
        let conv2d16_out1 = self.conv2d16.forward(relu5_out1);
        let relu6_out1 = burn::tensor::activation::relu(conv2d16_out1);
        let conv2d17_out1 = self.conv2d17.forward(relu6_out1);
        let add41_out1 = conv2d17_out1.add(add40_out1);
        let unsqueeze86_out1 = [gather53_out1 as i64];
        let unsqueeze87_out1 = [gather54_out1 as i64];
        let concat63_out1: [i64; 2usize] = [&unsqueeze86_out1[..], &unsqueeze87_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let shape59_out1: [i64; 4] = {
            let axes = &add41_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice8_out1: [i64; 2] = shape59_out1[0..2].try_into().unwrap();
        let concat64_out1: [i64; 4usize] = [&slice8_out1[..], &concat63_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let resize3_out1 = {
            let target_height = concat64_out1[2] as usize;
            let target_width = concat64_out1[3] as usize;
            burn::tensor::module::interpolate(
                add41_out1,
                [target_height, target_width],
                burn::tensor::ops::InterpolateOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_align_corners(true),
            )
        };
        let conv2d18_out1 = self.conv2d18.forward(resize3_out1);
        let shape60_out1: [i64; 4] = {
            let axes = &conv2d7_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather55_out1 = shape60_out1[2] as i64;
        let gather56_out1 = shape60_out1[3] as i64;
        let relu7_out1 = burn::tensor::activation::relu(conv2d8_out1.clone());
        let conv2d19_out1 = self.conv2d19.forward(relu7_out1);
        let relu8_out1 = burn::tensor::activation::relu(conv2d19_out1);
        let conv2d20_out1 = self.conv2d20.forward(relu8_out1);
        let add42_out1 = conv2d20_out1.add(conv2d8_out1);
        let add43_out1 = conv2d18_out1.add(add42_out1);
        let relu9_out1 = burn::tensor::activation::relu(add43_out1.clone());
        let conv2d21_out1 = self.conv2d21.forward(relu9_out1);
        let relu10_out1 = burn::tensor::activation::relu(conv2d21_out1);
        let conv2d22_out1 = self.conv2d22.forward(relu10_out1);
        let add44_out1 = conv2d22_out1.add(add43_out1);
        let unsqueeze88_out1 = [gather55_out1 as i64];
        let unsqueeze89_out1 = [gather56_out1 as i64];
        let concat65_out1: [i64; 2usize] = [&unsqueeze88_out1[..], &unsqueeze89_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let shape62_out1: [i64; 4] = {
            let axes = &add44_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice9_out1: [i64; 2] = shape62_out1[0..2].try_into().unwrap();
        let concat66_out1: [i64; 4usize] = [&slice9_out1[..], &concat65_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let resize4_out1 = {
            let target_height = concat66_out1[2] as usize;
            let target_width = concat66_out1[3] as usize;
            burn::tensor::module::interpolate(
                add44_out1,
                [target_height, target_width],
                burn::tensor::ops::InterpolateOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_align_corners(true),
            )
        };
        let conv2d23_out1 = self.conv2d23.forward(resize4_out1);
        let relu11_out1 = burn::tensor::activation::relu(conv2d7_out1.clone());
        let conv2d24_out1 = self.conv2d24.forward(relu11_out1);
        let relu12_out1 = burn::tensor::activation::relu(conv2d24_out1);
        let conv2d25_out1 = self.conv2d25.forward(relu12_out1);
        let add45_out1 = conv2d25_out1.add(conv2d7_out1);
        let add46_out1 = conv2d23_out1.add(add45_out1);
        let relu13_out1 = burn::tensor::activation::relu(add46_out1.clone());
        let conv2d26_out1 = self.conv2d26.forward(relu13_out1);
        let relu14_out1 = burn::tensor::activation::relu(conv2d26_out1);
        let conv2d27_out1 = self.conv2d27.forward(relu14_out1);
        let add47_out1 = conv2d27_out1.add(add46_out1);
        let resize5_out1 = self.resize5.forward(add47_out1);
        let conv2d28_out1 = self.conv2d28.forward(resize5_out1);
        let conv2d29_out1 = self.conv2d29.forward(conv2d28_out1);
        let shape63_out1: [i64; 4] = {
            let axes = &conv2d29_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice10_out1: [i64; 2] = shape63_out1[0..2].try_into().unwrap();
        let constant658_out1: [i64; 2] = [518i64, 518i64];
        let concat67_out1: [i64; 4usize] = [&slice10_out1[..], &constant658_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let resize6_out1 = {
            let target_height = concat67_out1[2] as usize;
            let target_width = concat67_out1[3] as usize;
            burn::tensor::module::interpolate(
                conv2d29_out1,
                [target_height, target_width],
                burn::tensor::ops::InterpolateOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_align_corners(true),
            )
        };
        let conv2d30_out1 = self.conv2d30.forward(resize6_out1);
        let relu15_out1 = burn::tensor::activation::relu(conv2d30_out1);
        let conv2d31_out1 = self.conv2d31.forward(relu15_out1);
        let sigmoid1_out1 = burn::tensor::activation::sigmoid(conv2d31_out1);
        let constant659_out1 = self.constant659.val();
        let mul74_out1 = sigmoid1_out1
            .mul((constant659_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let squeeze1_out1 = mul74_out1.squeeze_dims::<3>(&[1]);
        squeeze1_out1
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
        Self::from_file(
            "/home/critix/repos/rust/TentaFlow/.runtime/models/vision/depth-anything-v2-metric.bpk",
            &Default::default(),
        )
    }
}

impl<B: Backend> Model<B> {
    /// Load model weights from a burnpack file.
    pub fn from_file<P: AsRef<std::path::Path>>(file: P, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_file(file);
        model.load_from(&mut store).expect("Failed to load burnpack file");
        model
    }

    /// Load model weights from in-memory bytes.
    ///
    /// The bytes must be the contents of a `.bpk` file.
    pub fn from_bytes(bytes: Bytes, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_bytes(Some(bytes));
        model.load_from(&mut store).expect("Failed to load burnpack bytes");
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
    pub fn forward(&self, pixel_values: Tensor<B, 4>) -> Tensor<B, 3> {
        let (add2_out1, shape1_out1) = self.submodule1.forward(pixel_values);
        let add7_out1 = self.submodule2.forward(add2_out1);
        let (matmul29_out1, add10_out1) = self.submodule3.forward(add7_out1);
        let add14_out1 = self.submodule4.forward(matmul29_out1, add10_out1.clone());
        let add19_out1 = self.submodule5.forward(add14_out1);
        let add23_out1 = self.submodule6.forward(add19_out1.clone());
        let add28_out1 = self.submodule7.forward(add23_out1);
        let (
            reshape56_out1,
            convtranspose2d1_out1,
            convtranspose2d2_out1,
            conv2d4_out1,
        ) = self.submodule8.forward(add28_out1, add10_out1, add19_out1, shape1_out1);
        let squeeze1_out1 = self
            .submodule9
            .forward(
                reshape56_out1,
                convtranspose2d1_out1,
                convtranspose2d2_out1,
                conv2d4_out1,
            );
        squeeze1_out1
    }
}
