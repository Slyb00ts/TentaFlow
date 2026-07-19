// Generated from ONNX "/home/critix/repos/rust/TentaFlow/.runtime/models/vision/rfdetr-base.onnx" by burn-onnx
use burn::prelude::*;
use burn::nn::LayerNorm;
use burn::nn::LayerNormConfig;
use burn::nn::Linear;
use burn::nn::LinearConfig;
use burn::nn::LinearLayout;
use burn::nn::PaddingConfig2d;
use burn::nn::conv::Conv2d;
use burn::nn::conv::Conv2dConfig;
use burn::tensor::Bytes;
use burn::tensor::TensorData;
use burn_store::BurnpackStore;
use burn_store::ModuleSnapshot;


#[derive(Module, Debug)]
pub struct Submodule1<B: Backend> {
    conv2d1: Conv2d<B>,
    constant352: burn::module::Param<Tensor<B, 1, Int>>,
    constant58: burn::module::Param<Tensor<B, 3>>,
    constant59: burn::module::Param<Tensor<B, 3>>,
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
        let constant352: burn::module::Param<Tensor<B, 1, Int>> = burn::module::Param::uninitialized(
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
        let constant58: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1, 384].into(),
        );
        let constant59: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1601, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1601, 384].into(),
        );
        Self {
            conv2d1,
            constant352,
            constant58,
            constant59,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, input: Tensor<B, 4>) -> (Tensor<B, 3>, [i64; 4]) {
        let shape1_out1: [i64; 4] = {
            let axes = &input.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather1_out1 = shape1_out1[0] as i64;
        let gather2_out1 = shape1_out1[2] as i64;
        let gather3_out1 = shape1_out1[3] as i64;
        let conv2d1_out1 = self.conv2d1.forward(input);
        let shape4_out1: [i64; 4] = {
            let axes = &conv2d1_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice1_out1: [i64; 2] = shape4_out1[0..2].try_into().unwrap();
        let constant347_out1: [i64; 1] = [-1i64];
        let concat1_out1: [i64; 3usize] = [&slice1_out1[..], &constant347_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape1_out1 = conv2d1_out1.reshape(concat1_out1);
        let transpose1_out1 = reshape1_out1.permute([0, 2, 1]);
        let unsqueeze1_out1 = [gather1_out1 as i64];
        let constant350_out1: [i64; 1] = [-1i64];
        let constant349_out1: [i64; 1] = [-1i64];
        let concat2_out1: [i64; 3usize] = [
            &unsqueeze1_out1[..],
            &constant349_out1[..],
            &constant350_out1[..],
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
        let constant352_out1 = self.constant352.val();
        let mul1_out1 = constantofshape1_out1.clone().mul(constant352_out1);
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
        let constant58_out1 = self.constant58.val();
        let expand1_out1 = {
            let onnx_shape: [i64; 3usize] = TryInto::<
                [i64; 3usize],
            >::try_into(where1_out1.to_data().convert::<i64>().as_slice().unwrap())
                .unwrap();
            let input_dims = constant58_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            constant58_out1.expand(shape)
        };
        let concat3_out1 = burn::tensor::Tensor::cat(
            [expand1_out1, transpose1_out1].into(),
            1,
        );
        let constant59_out1 = self.constant59.val();
        let add1_out1 = concat3_out1.add(constant59_out1);
        let constant353_out1 = 14i64;
        let div1_out1 = gather2_out1 / constant353_out1;
        let constant354_out1 = 14i64;
        let div2_out1 = gather3_out1 / constant354_out1;
        let slice2_out1 = add1_out1.clone().slice(s![.., 0..1, ..]);
        let slice3_out1 = add1_out1.slice(s![.., 1.., ..]);
        let unsqueeze2_out1 = [gather1_out1 as i64];
        let unsqueeze3_out1 = [div1_out1 as i64];
        let unsqueeze4_out1 = [div2_out1 as i64];
        let constant366_out1: [i64; 1] = [-1i64];
        let concat4_out1: [i64; 4usize] = [
            &unsqueeze2_out1[..],
            &unsqueeze3_out1[..],
            &unsqueeze4_out1[..],
            &constant366_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape3_out1 = slice3_out1.reshape(concat4_out1);
        let constant367_out1 = 4i64;
        let div3_out1 = div2_out1 / constant367_out1;
        let constant368_out1 = 4i64;
        let div4_out1 = div1_out1 / constant368_out1;
        let constant369_out1 = 4i64;
        let mul2_out1 = gather1_out1 * constant369_out1;
        let unsqueeze5_out1 = [mul2_out1 as i64];
        let unsqueeze6_out1 = [div4_out1 as i64];
        let unsqueeze7_out1 = [div3_out1 as i64];
        let constant374_out1: [i64; 1] = [-1i64];
        let constant372_out1: [i64; 1] = [4i64];
        let concat5_out1: [i64; 5usize] = [
            &unsqueeze5_out1[..],
            &unsqueeze6_out1[..],
            &constant372_out1[..],
            &unsqueeze7_out1[..],
            &constant374_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape4_out1 = reshape3_out1.reshape(concat5_out1);
        let transpose2_out1 = reshape4_out1.permute([0, 2, 1, 3, 4]);
        let constant375_out1 = 16i64;
        let mul3_out1 = gather1_out1 * constant375_out1;
        let mul4_out1 = div4_out1 * div3_out1;
        let unsqueeze8_out1 = [mul3_out1 as i64];
        let unsqueeze9_out1 = [mul4_out1 as i64];
        let constant378_out1: [i64; 1] = [-1i64];
        let concat6_out1: [i64; 3usize] = [
            &unsqueeze8_out1[..],
            &unsqueeze9_out1[..],
            &constant378_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape5_out1 = transpose2_out1.reshape(concat6_out1);
        let constantofshape2_out1: [i64; 3usize] = [1i64, 1i64, 1i64];
        let expand2_out1 = {
            let onnx_shape: [i64; 3usize] = constantofshape2_out1;
            let input_dims = slice2_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            slice2_out1.expand(shape)
        };
        let tile1_out1 = expand2_out1.repeat(&[16, 1, 1]);
        let concat7_out1 = burn::tensor::Tensor::cat(
            [tile1_out1, reshape5_out1].into(),
            1,
        );
        (concat7_out1, shape1_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule2<B: Backend> {
    layernormalization1: LayerNorm<B>,
    linear1: Linear<B>,
    linear2: Linear<B>,
    linear3: Linear<B>,
    linear4: Linear<B>,
    constant68: burn::module::Param<Tensor<B, 1>>,
    layernormalization2: LayerNorm<B>,
    linear5: Linear<B>,
    constant407: burn::module::Param<Tensor<B, 1>>,
    constant408: burn::module::Param<Tensor<B, 1>>,
    constant409: burn::module::Param<Tensor<B, 1>>,
    linear6: Linear<B>,
    constant73: burn::module::Param<Tensor<B, 1>>,
    layernormalization3: LayerNorm<B>,
    linear7: Linear<B>,
    linear8: Linear<B>,
    linear9: Linear<B>,
    linear10: Linear<B>,
    constant80: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule2<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization1 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear1 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear2 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear3 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear4 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant68: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization2 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear5 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant407: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant408: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant409: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant73: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear10 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant80: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            layernormalization1,
            linear1,
            linear2,
            linear3,
            linear4,
            constant68,
            layernormalization2,
            linear5,
            constant407,
            constant408,
            constant409,
            linear6,
            constant73,
            layernormalization3,
            linear7,
            linear8,
            linear9,
            linear10,
            constant80,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, concat7_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization1_out1 = {
            let dtype = concat7_out1.clone().dtype();
            self.layernormalization1
                .forward(concat7_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear1_out1 = self.linear1.forward(layernormalization1_out1.clone());
        let linear2_out1 = self.linear2.forward(layernormalization1_out1.clone());
        let shape6_out1: [i64; 3] = {
            let axes = &linear2_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather4_out1 = shape6_out1[0] as i64;
        let gather5_out1 = shape6_out1[1] as i64;
        let unsqueeze10_out1 = [gather4_out1 as i64];
        let unsqueeze11_out1 = [gather5_out1 as i64];
        let constant386_out1: [i64; 1] = [64i64];
        let constant385_out1: [i64; 1] = [6i64];
        let concat8_out1: [i64; 4usize] = [
            &unsqueeze10_out1[..],
            &unsqueeze11_out1[..],
            &constant385_out1[..],
            &constant386_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape6_out1 = linear2_out1.reshape(concat8_out1);
        let linear3_out1 = self.linear3.forward(layernormalization1_out1);
        let shape8_out1: [i64; 3] = {
            let axes = &linear3_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather6_out1 = shape8_out1[0] as i64;
        let gather7_out1 = shape8_out1[1] as i64;
        let unsqueeze12_out1 = [gather6_out1 as i64];
        let unsqueeze13_out1 = [gather7_out1 as i64];
        let constant392_out1: [i64; 1] = [64i64];
        let constant391_out1: [i64; 1] = [6i64];
        let concat9_out1: [i64; 4usize] = [
            &unsqueeze12_out1[..],
            &unsqueeze13_out1[..],
            &constant391_out1[..],
            &constant392_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape7_out1 = linear3_out1.reshape(concat9_out1);
        let transpose3_out1 = reshape7_out1.permute([0, 2, 1, 3]);
        let shape10_out1: [i64; 3] = {
            let axes = &linear1_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather8_out1 = shape10_out1[0] as i64;
        let gather9_out1 = shape10_out1[1] as i64;
        let unsqueeze14_out1 = [gather8_out1 as i64];
        let unsqueeze15_out1 = [gather9_out1 as i64];
        let constant398_out1: [i64; 1] = [64i64];
        let constant397_out1: [i64; 1] = [6i64];
        let concat10_out1: [i64; 4usize] = [
            &unsqueeze14_out1[..],
            &unsqueeze15_out1[..],
            &constant397_out1[..],
            &constant398_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape8_out1 = linear1_out1.reshape(concat10_out1);
        let transpose4_out1 = reshape8_out1.permute([0, 2, 1, 3]);
        let transpose5_out1 = reshape6_out1.permute([0, 2, 3, 1]);
        let matmul4_k_corrected = transpose5_out1.permute([0, 1, 3, 2]);
        let (matmul5_out1,) = {
            let q = transpose4_out1;
            let k = matmul4_k_corrected;
            let v = transpose3_out1;
            let matmul5_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul5_out1,)
        };
        let transpose6_out1 = matmul5_out1.permute([0, 2, 1, 3]);
        let shape13_out1: [i64; 4] = {
            let axes = &transpose6_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather10_out1 = shape13_out1[0] as i64;
        let gather11_out1 = shape13_out1[1] as i64;
        let unsqueeze16_out1 = [gather10_out1 as i64];
        let unsqueeze17_out1 = [gather11_out1 as i64];
        let constant406_out1: [i64; 1] = [384i64];
        let concat11_out1: [i64; 3usize] = [
            &unsqueeze16_out1[..],
            &unsqueeze17_out1[..],
            &constant406_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape9_out1 = transpose6_out1.reshape(concat11_out1);
        let linear4_out1 = self.linear4.forward(reshape9_out1);
        let constant68_out1 = self.constant68.val();
        let mul7_out1 = linear4_out1
            .mul((constant68_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add2_out1 = mul7_out1.add(concat7_out1);
        let layernormalization2_out1 = {
            let dtype = add2_out1.clone().dtype();
            self.layernormalization2
                .forward(add2_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear5_out1 = self.linear5.forward(layernormalization2_out1);
        let constant407_out1 = self.constant407.val();
        let div6_out1 = linear5_out1
            .clone()
            .div((constant407_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf1_out1 = div6_out1.erf();
        let constant408_out1 = self.constant408.val();
        let add3_out1 = erf1_out1
            .add((constant408_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul8_out1 = linear5_out1.mul(add3_out1);
        let constant409_out1 = self.constant409.val();
        let mul9_out1 = mul8_out1
            .mul((constant409_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear6_out1 = self.linear6.forward(mul9_out1);
        let constant73_out1 = self.constant73.val();
        let mul10_out1 = linear6_out1
            .mul((constant73_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add4_out1 = mul10_out1.add(add2_out1);
        let layernormalization3_out1 = {
            let dtype = add4_out1.clone().dtype();
            self.layernormalization3
                .forward(add4_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear7_out1 = self.linear7.forward(layernormalization3_out1.clone());
        let linear8_out1 = self.linear8.forward(layernormalization3_out1.clone());
        let shape15_out1: [i64; 3] = {
            let axes = &linear8_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather12_out1 = shape15_out1[0] as i64;
        let gather13_out1 = shape15_out1[1] as i64;
        let unsqueeze18_out1 = [gather12_out1 as i64];
        let unsqueeze19_out1 = [gather13_out1 as i64];
        let constant415_out1: [i64; 1] = [64i64];
        let constant414_out1: [i64; 1] = [6i64];
        let concat12_out1: [i64; 4usize] = [
            &unsqueeze18_out1[..],
            &unsqueeze19_out1[..],
            &constant414_out1[..],
            &constant415_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape10_out1 = linear8_out1.reshape(concat12_out1);
        let linear9_out1 = self.linear9.forward(layernormalization3_out1);
        let shape17_out1: [i64; 3] = {
            let axes = &linear9_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather14_out1 = shape17_out1[0] as i64;
        let gather15_out1 = shape17_out1[1] as i64;
        let unsqueeze20_out1 = [gather14_out1 as i64];
        let unsqueeze21_out1 = [gather15_out1 as i64];
        let constant421_out1: [i64; 1] = [64i64];
        let constant420_out1: [i64; 1] = [6i64];
        let concat13_out1: [i64; 4usize] = [
            &unsqueeze20_out1[..],
            &unsqueeze21_out1[..],
            &constant420_out1[..],
            &constant421_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape11_out1 = linear9_out1.reshape(concat13_out1);
        let transpose7_out1 = reshape11_out1.permute([0, 2, 1, 3]);
        let shape19_out1: [i64; 3] = {
            let axes = &linear7_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather16_out1 = shape19_out1[0] as i64;
        let gather17_out1 = shape19_out1[1] as i64;
        let unsqueeze22_out1 = [gather16_out1 as i64];
        let unsqueeze23_out1 = [gather17_out1 as i64];
        let constant427_out1: [i64; 1] = [64i64];
        let constant426_out1: [i64; 1] = [6i64];
        let concat14_out1: [i64; 4usize] = [
            &unsqueeze22_out1[..],
            &unsqueeze23_out1[..],
            &constant426_out1[..],
            &constant427_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape12_out1 = linear7_out1.reshape(concat14_out1);
        let transpose8_out1 = reshape12_out1.permute([0, 2, 1, 3]);
        let transpose9_out1 = reshape10_out1.permute([0, 2, 3, 1]);
        let matmul12_k_corrected = transpose9_out1.permute([0, 1, 3, 2]);
        let (matmul13_out1,) = {
            let q = transpose8_out1;
            let k = matmul12_k_corrected;
            let v = transpose7_out1;
            let matmul13_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul13_out1,)
        };
        let transpose10_out1 = matmul13_out1.permute([0, 2, 1, 3]);
        let shape22_out1: [i64; 4] = {
            let axes = &transpose10_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather18_out1 = shape22_out1[0] as i64;
        let gather19_out1 = shape22_out1[1] as i64;
        let unsqueeze24_out1 = [gather18_out1 as i64];
        let unsqueeze25_out1 = [gather19_out1 as i64];
        let constant435_out1: [i64; 1] = [384i64];
        let concat15_out1: [i64; 3usize] = [
            &unsqueeze24_out1[..],
            &unsqueeze25_out1[..],
            &constant435_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape13_out1 = transpose10_out1.reshape(concat15_out1);
        let linear10_out1 = self.linear10.forward(reshape13_out1);
        let constant80_out1 = self.constant80.val();
        let mul13_out1 = linear10_out1
            .mul((constant80_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add5_out1 = mul13_out1.add(add4_out1);
        add5_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule3<B: Backend> {
    layernormalization4: LayerNorm<B>,
    linear11: Linear<B>,
    constant436: burn::module::Param<Tensor<B, 1>>,
    constant437: burn::module::Param<Tensor<B, 1>>,
    constant438: burn::module::Param<Tensor<B, 1>>,
    linear12: Linear<B>,
    constant85: burn::module::Param<Tensor<B, 1>>,
    layernormalization5: LayerNorm<B>,
    linear13: Linear<B>,
    linear14: Linear<B>,
    linear15: Linear<B>,
    linear16: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule3<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization4 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear11 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant436: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant437: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant438: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant85: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization5 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear13 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear14 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear15 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear16 = LinearConfig::new(384, 384).with_bias(true).init(device);
        Self {
            layernormalization4,
            linear11,
            constant436,
            constant437,
            constant438,
            linear12,
            constant85,
            layernormalization5,
            linear13,
            linear14,
            linear15,
            linear16,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add5_out1: Tensor<B, 3>) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let layernormalization4_out1 = {
            let dtype = add5_out1.clone().dtype();
            self.layernormalization4
                .forward(add5_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear11_out1 = self.linear11.forward(layernormalization4_out1);
        let constant436_out1 = self.constant436.val();
        let div8_out1 = linear11_out1
            .clone()
            .div((constant436_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf2_out1 = div8_out1.erf();
        let constant437_out1 = self.constant437.val();
        let add6_out1 = erf2_out1
            .add((constant437_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul14_out1 = linear11_out1.mul(add6_out1);
        let constant438_out1 = self.constant438.val();
        let mul15_out1 = mul14_out1
            .mul((constant438_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear12_out1 = self.linear12.forward(mul15_out1);
        let constant85_out1 = self.constant85.val();
        let mul16_out1 = linear12_out1
            .mul((constant85_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add7_out1 = mul16_out1.add(add5_out1);
        let shape24_out1: [i64; 3] = {
            let axes = &add7_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather20_out1 = shape24_out1[0] as i64;
        let gather21_out1 = shape24_out1[1] as i64;
        let gather22_out1 = shape24_out1[2] as i64;
        let constant442_out1 = 16i64;
        let div9_out1 = gather20_out1 / constant442_out1;
        let constant443_out1 = 16i64;
        let mul17_out1 = gather21_out1 * constant443_out1;
        let unsqueeze26_out1 = [div9_out1 as i64];
        let unsqueeze27_out1 = [mul17_out1 as i64];
        let unsqueeze28_out1 = [gather22_out1 as i64];
        let concat16_out1: [i64; 3usize] = [
            &unsqueeze26_out1[..],
            &unsqueeze27_out1[..],
            &unsqueeze28_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape14_out1 = add7_out1.clone().reshape(concat16_out1);
        let layernormalization5_out1 = {
            let dtype = reshape14_out1.clone().dtype();
            self.layernormalization5
                .forward(reshape14_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear13_out1 = self.linear13.forward(layernormalization5_out1.clone());
        let linear14_out1 = self.linear14.forward(layernormalization5_out1.clone());
        let shape27_out1: [i64; 3] = {
            let axes = &linear14_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather23_out1 = shape27_out1[0] as i64;
        let gather24_out1 = shape27_out1[1] as i64;
        let unsqueeze29_out1 = [gather23_out1 as i64];
        let unsqueeze30_out1 = [gather24_out1 as i64];
        let constant452_out1: [i64; 1] = [64i64];
        let constant451_out1: [i64; 1] = [6i64];
        let concat17_out1: [i64; 4usize] = [
            &unsqueeze29_out1[..],
            &unsqueeze30_out1[..],
            &constant451_out1[..],
            &constant452_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape15_out1 = linear14_out1.reshape(concat17_out1);
        let linear15_out1 = self.linear15.forward(layernormalization5_out1);
        let shape29_out1: [i64; 3] = {
            let axes = &linear15_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather25_out1 = shape29_out1[0] as i64;
        let gather26_out1 = shape29_out1[1] as i64;
        let unsqueeze31_out1 = [gather25_out1 as i64];
        let unsqueeze32_out1 = [gather26_out1 as i64];
        let constant458_out1: [i64; 1] = [64i64];
        let constant457_out1: [i64; 1] = [6i64];
        let concat18_out1: [i64; 4usize] = [
            &unsqueeze31_out1[..],
            &unsqueeze32_out1[..],
            &constant457_out1[..],
            &constant458_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape16_out1 = linear15_out1.reshape(concat18_out1);
        let transpose11_out1 = reshape16_out1.permute([0, 2, 1, 3]);
        let shape31_out1: [i64; 3] = {
            let axes = &linear13_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather27_out1 = shape31_out1[0] as i64;
        let gather28_out1 = shape31_out1[1] as i64;
        let unsqueeze33_out1 = [gather27_out1 as i64];
        let unsqueeze34_out1 = [gather28_out1 as i64];
        let constant464_out1: [i64; 1] = [64i64];
        let constant463_out1: [i64; 1] = [6i64];
        let concat19_out1: [i64; 4usize] = [
            &unsqueeze33_out1[..],
            &unsqueeze34_out1[..],
            &constant463_out1[..],
            &constant464_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape17_out1 = linear13_out1.reshape(concat19_out1);
        let transpose12_out1 = reshape17_out1.permute([0, 2, 1, 3]);
        let transpose13_out1 = reshape15_out1.permute([0, 2, 3, 1]);
        let matmul20_k_corrected = transpose13_out1.permute([0, 1, 3, 2]);
        let (matmul21_out1,) = {
            let q = transpose12_out1;
            let k = matmul20_k_corrected;
            let v = transpose11_out1;
            let matmul21_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul21_out1,)
        };
        let transpose14_out1 = matmul21_out1.permute([0, 2, 1, 3]);
        let shape34_out1: [i64; 4] = {
            let axes = &transpose14_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather29_out1 = shape34_out1[0] as i64;
        let gather30_out1 = shape34_out1[1] as i64;
        let unsqueeze35_out1 = [gather29_out1 as i64];
        let unsqueeze36_out1 = [gather30_out1 as i64];
        let constant472_out1: [i64; 1] = [384i64];
        let concat20_out1: [i64; 3usize] = [
            &unsqueeze35_out1[..],
            &unsqueeze36_out1[..],
            &constant472_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape18_out1 = transpose14_out1.reshape(concat20_out1);
        let linear16_out1 = self.linear16.forward(reshape18_out1);
        let shape36_out1: [i64; 3] = {
            let axes = &reshape14_out1.dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather31_out1 = shape36_out1[0] as i64;
        let gather32_out1 = shape36_out1[1] as i64;
        let gather33_out1 = shape36_out1[2] as i64;
        let constant476_out1 = 16i64;
        let mul20_out1 = gather31_out1 * constant476_out1;
        let constant477_out1 = 16i64;
        let div11_out1 = gather32_out1 / constant477_out1;
        let unsqueeze37_out1 = [mul20_out1 as i64];
        let unsqueeze38_out1 = [div11_out1 as i64];
        let unsqueeze39_out1 = [gather33_out1 as i64];
        let concat21_out1: [i64; 3usize] = [
            &unsqueeze37_out1[..],
            &unsqueeze38_out1[..],
            &unsqueeze39_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape19_out1 = linear16_out1.reshape(concat21_out1);
        (reshape19_out1, add7_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule4<B: Backend> {
    constant92: burn::module::Param<Tensor<B, 1>>,
    layernormalization6: LayerNorm<B>,
    linear17: Linear<B>,
    constant481: burn::module::Param<Tensor<B, 1>>,
    constant482: burn::module::Param<Tensor<B, 1>>,
    constant483: burn::module::Param<Tensor<B, 1>>,
    linear18: Linear<B>,
    constant97: burn::module::Param<Tensor<B, 1>>,
    layernormalization7: LayerNorm<B>,
    linear19: Linear<B>,
    linear20: Linear<B>,
    linear21: Linear<B>,
    linear22: Linear<B>,
    constant104: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule4<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant92: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant481: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant482: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant483: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant97: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear22 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant104: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            constant92,
            layernormalization6,
            linear17,
            constant481,
            constant482,
            constant483,
            linear18,
            constant97,
            layernormalization7,
            linear19,
            linear20,
            linear21,
            linear22,
            constant104,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        reshape19_out1: Tensor<B, 3>,
        add7_out1: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let constant92_out1 = self.constant92.val();
        let mul21_out1 = reshape19_out1
            .mul((constant92_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add8_out1 = mul21_out1.add(add7_out1);
        let layernormalization6_out1 = {
            let dtype = add8_out1.clone().dtype();
            self.layernormalization6
                .forward(add8_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear17_out1 = self.linear17.forward(layernormalization6_out1);
        let constant481_out1 = self.constant481.val();
        let div12_out1 = linear17_out1
            .clone()
            .div((constant481_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf3_out1 = div12_out1.erf();
        let constant482_out1 = self.constant482.val();
        let add9_out1 = erf3_out1
            .add((constant482_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul22_out1 = linear17_out1.mul(add9_out1);
        let constant483_out1 = self.constant483.val();
        let mul23_out1 = mul22_out1
            .mul((constant483_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear18_out1 = self.linear18.forward(mul23_out1);
        let constant97_out1 = self.constant97.val();
        let mul24_out1 = linear18_out1
            .mul((constant97_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add10_out1 = mul24_out1.add(add8_out1);
        let layernormalization7_out1 = {
            let dtype = add10_out1.clone().dtype();
            self.layernormalization7
                .forward(add10_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear19_out1 = self.linear19.forward(layernormalization7_out1.clone());
        let linear20_out1 = self.linear20.forward(layernormalization7_out1.clone());
        let shape39_out1: [i64; 3] = {
            let axes = &linear20_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather34_out1 = shape39_out1[0] as i64;
        let gather35_out1 = shape39_out1[1] as i64;
        let unsqueeze40_out1 = [gather34_out1 as i64];
        let unsqueeze41_out1 = [gather35_out1 as i64];
        let constant489_out1: [i64; 1] = [64i64];
        let constant488_out1: [i64; 1] = [6i64];
        let concat22_out1: [i64; 4usize] = [
            &unsqueeze40_out1[..],
            &unsqueeze41_out1[..],
            &constant488_out1[..],
            &constant489_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape20_out1 = linear20_out1.reshape(concat22_out1);
        let linear21_out1 = self.linear21.forward(layernormalization7_out1);
        let shape41_out1: [i64; 3] = {
            let axes = &linear21_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather36_out1 = shape41_out1[0] as i64;
        let gather37_out1 = shape41_out1[1] as i64;
        let unsqueeze42_out1 = [gather36_out1 as i64];
        let unsqueeze43_out1 = [gather37_out1 as i64];
        let constant495_out1: [i64; 1] = [64i64];
        let constant494_out1: [i64; 1] = [6i64];
        let concat23_out1: [i64; 4usize] = [
            &unsqueeze42_out1[..],
            &unsqueeze43_out1[..],
            &constant494_out1[..],
            &constant495_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape21_out1 = linear21_out1.reshape(concat23_out1);
        let transpose15_out1 = reshape21_out1.permute([0, 2, 1, 3]);
        let shape43_out1: [i64; 3] = {
            let axes = &linear19_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather38_out1 = shape43_out1[0] as i64;
        let gather39_out1 = shape43_out1[1] as i64;
        let unsqueeze44_out1 = [gather38_out1 as i64];
        let unsqueeze45_out1 = [gather39_out1 as i64];
        let constant501_out1: [i64; 1] = [64i64];
        let constant500_out1: [i64; 1] = [6i64];
        let concat24_out1: [i64; 4usize] = [
            &unsqueeze44_out1[..],
            &unsqueeze45_out1[..],
            &constant500_out1[..],
            &constant501_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape22_out1 = linear19_out1.reshape(concat24_out1);
        let transpose16_out1 = reshape22_out1.permute([0, 2, 1, 3]);
        let transpose17_out1 = reshape20_out1.permute([0, 2, 3, 1]);
        let matmul28_k_corrected = transpose17_out1.permute([0, 1, 3, 2]);
        let (matmul29_out1,) = {
            let q = transpose16_out1;
            let k = matmul28_k_corrected;
            let v = transpose15_out1;
            let matmul29_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul29_out1,)
        };
        let transpose18_out1 = matmul29_out1.permute([0, 2, 1, 3]);
        let shape46_out1: [i64; 4] = {
            let axes = &transpose18_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather40_out1 = shape46_out1[0] as i64;
        let gather41_out1 = shape46_out1[1] as i64;
        let unsqueeze46_out1 = [gather40_out1 as i64];
        let unsqueeze47_out1 = [gather41_out1 as i64];
        let constant509_out1: [i64; 1] = [384i64];
        let concat25_out1: [i64; 3usize] = [
            &unsqueeze46_out1[..],
            &unsqueeze47_out1[..],
            &constant509_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape23_out1 = transpose18_out1.reshape(concat25_out1);
        let linear22_out1 = self.linear22.forward(reshape23_out1);
        let constant104_out1 = self.constant104.val();
        let mul27_out1 = linear22_out1
            .mul((constant104_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add11_out1 = mul27_out1.add(add10_out1);
        add11_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule5<B: Backend> {
    layernormalization8: LayerNorm<B>,
    linear23: Linear<B>,
    constant510: burn::module::Param<Tensor<B, 1>>,
    constant511: burn::module::Param<Tensor<B, 1>>,
    constant512: burn::module::Param<Tensor<B, 1>>,
    linear24: Linear<B>,
    constant109: burn::module::Param<Tensor<B, 1>>,
    layernormalization9: LayerNorm<B>,
    linear25: Linear<B>,
    linear26: Linear<B>,
    linear27: Linear<B>,
    linear28: Linear<B>,
    constant116: burn::module::Param<Tensor<B, 1>>,
    layernormalization10: LayerNorm<B>,
    linear29: Linear<B>,
    constant539: burn::module::Param<Tensor<B, 1>>,
    constant540: burn::module::Param<Tensor<B, 1>>,
    constant541: burn::module::Param<Tensor<B, 1>>,
    linear30: Linear<B>,
    constant121: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule5<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization8 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear23 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant510: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant511: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant512: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant109: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear28 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant116: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization10 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear29 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant539: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant540: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant541: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant121: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            layernormalization8,
            linear23,
            constant510,
            constant511,
            constant512,
            linear24,
            constant109,
            layernormalization9,
            linear25,
            linear26,
            linear27,
            linear28,
            constant116,
            layernormalization10,
            linear29,
            constant539,
            constant540,
            constant541,
            linear30,
            constant121,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add11_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization8_out1 = {
            let dtype = add11_out1.clone().dtype();
            self.layernormalization8
                .forward(add11_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear23_out1 = self.linear23.forward(layernormalization8_out1);
        let constant510_out1 = self.constant510.val();
        let div14_out1 = linear23_out1
            .clone()
            .div((constant510_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf4_out1 = div14_out1.erf();
        let constant511_out1 = self.constant511.val();
        let add12_out1 = erf4_out1
            .add((constant511_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul28_out1 = linear23_out1.mul(add12_out1);
        let constant512_out1 = self.constant512.val();
        let mul29_out1 = mul28_out1
            .mul((constant512_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear24_out1 = self.linear24.forward(mul29_out1);
        let constant109_out1 = self.constant109.val();
        let mul30_out1 = linear24_out1
            .mul((constant109_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add13_out1 = mul30_out1.add(add11_out1);
        let layernormalization9_out1 = {
            let dtype = add13_out1.clone().dtype();
            self.layernormalization9
                .forward(add13_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear25_out1 = self.linear25.forward(layernormalization9_out1.clone());
        let linear26_out1 = self.linear26.forward(layernormalization9_out1.clone());
        let shape48_out1: [i64; 3] = {
            let axes = &linear26_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather42_out1 = shape48_out1[0] as i64;
        let gather43_out1 = shape48_out1[1] as i64;
        let unsqueeze48_out1 = [gather42_out1 as i64];
        let unsqueeze49_out1 = [gather43_out1 as i64];
        let constant518_out1: [i64; 1] = [64i64];
        let constant517_out1: [i64; 1] = [6i64];
        let concat26_out1: [i64; 4usize] = [
            &unsqueeze48_out1[..],
            &unsqueeze49_out1[..],
            &constant517_out1[..],
            &constant518_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape24_out1 = linear26_out1.reshape(concat26_out1);
        let linear27_out1 = self.linear27.forward(layernormalization9_out1);
        let shape50_out1: [i64; 3] = {
            let axes = &linear27_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather44_out1 = shape50_out1[0] as i64;
        let gather45_out1 = shape50_out1[1] as i64;
        let unsqueeze50_out1 = [gather44_out1 as i64];
        let unsqueeze51_out1 = [gather45_out1 as i64];
        let constant524_out1: [i64; 1] = [64i64];
        let constant523_out1: [i64; 1] = [6i64];
        let concat27_out1: [i64; 4usize] = [
            &unsqueeze50_out1[..],
            &unsqueeze51_out1[..],
            &constant523_out1[..],
            &constant524_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape25_out1 = linear27_out1.reshape(concat27_out1);
        let transpose19_out1 = reshape25_out1.permute([0, 2, 1, 3]);
        let shape52_out1: [i64; 3] = {
            let axes = &linear25_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather46_out1 = shape52_out1[0] as i64;
        let gather47_out1 = shape52_out1[1] as i64;
        let unsqueeze52_out1 = [gather46_out1 as i64];
        let unsqueeze53_out1 = [gather47_out1 as i64];
        let constant530_out1: [i64; 1] = [64i64];
        let constant529_out1: [i64; 1] = [6i64];
        let concat28_out1: [i64; 4usize] = [
            &unsqueeze52_out1[..],
            &unsqueeze53_out1[..],
            &constant529_out1[..],
            &constant530_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape26_out1 = linear25_out1.reshape(concat28_out1);
        let transpose20_out1 = reshape26_out1.permute([0, 2, 1, 3]);
        let transpose21_out1 = reshape24_out1.permute([0, 2, 3, 1]);
        let matmul36_k_corrected = transpose21_out1.permute([0, 1, 3, 2]);
        let (matmul37_out1,) = {
            let q = transpose20_out1;
            let k = matmul36_k_corrected;
            let v = transpose19_out1;
            let matmul37_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul37_out1,)
        };
        let transpose22_out1 = matmul37_out1.permute([0, 2, 1, 3]);
        let shape55_out1: [i64; 4] = {
            let axes = &transpose22_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather48_out1 = shape55_out1[0] as i64;
        let gather49_out1 = shape55_out1[1] as i64;
        let unsqueeze54_out1 = [gather48_out1 as i64];
        let unsqueeze55_out1 = [gather49_out1 as i64];
        let constant538_out1: [i64; 1] = [384i64];
        let concat29_out1: [i64; 3usize] = [
            &unsqueeze54_out1[..],
            &unsqueeze55_out1[..],
            &constant538_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape27_out1 = transpose22_out1.reshape(concat29_out1);
        let linear28_out1 = self.linear28.forward(reshape27_out1);
        let constant116_out1 = self.constant116.val();
        let mul33_out1 = linear28_out1
            .mul((constant116_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add14_out1 = mul33_out1.add(add13_out1);
        let layernormalization10_out1 = {
            let dtype = add14_out1.clone().dtype();
            self.layernormalization10
                .forward(add14_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear29_out1 = self.linear29.forward(layernormalization10_out1);
        let constant539_out1 = self.constant539.val();
        let div16_out1 = linear29_out1
            .clone()
            .div((constant539_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf5_out1 = div16_out1.erf();
        let constant540_out1 = self.constant540.val();
        let add15_out1 = erf5_out1
            .add((constant540_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul34_out1 = linear29_out1.mul(add15_out1);
        let constant541_out1 = self.constant541.val();
        let mul35_out1 = mul34_out1
            .mul((constant541_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear30_out1 = self.linear30.forward(mul35_out1);
        let constant121_out1 = self.constant121.val();
        let mul36_out1 = linear30_out1
            .mul((constant121_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add16_out1 = mul36_out1.add(add14_out1);
        add16_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule6<B: Backend> {
    layernormalization11: LayerNorm<B>,
    linear31: Linear<B>,
    linear32: Linear<B>,
    linear33: Linear<B>,
    linear34: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule6<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization11 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear31 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear32 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear33 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear34 = LinearConfig::new(384, 384).with_bias(true).init(device);
        Self {
            layernormalization11,
            linear31,
            linear32,
            linear33,
            linear34,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add16_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let shape57_out1: [i64; 3] = {
            let axes = &add16_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather50_out1 = shape57_out1[0] as i64;
        let gather51_out1 = shape57_out1[1] as i64;
        let gather52_out1 = shape57_out1[2] as i64;
        let constant545_out1 = 16i64;
        let div17_out1 = gather50_out1 / constant545_out1;
        let constant546_out1 = 16i64;
        let mul37_out1 = gather51_out1 * constant546_out1;
        let unsqueeze56_out1 = [div17_out1 as i64];
        let unsqueeze57_out1 = [mul37_out1 as i64];
        let unsqueeze58_out1 = [gather52_out1 as i64];
        let concat30_out1: [i64; 3usize] = [
            &unsqueeze56_out1[..],
            &unsqueeze57_out1[..],
            &unsqueeze58_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape28_out1 = add16_out1.reshape(concat30_out1);
        let layernormalization11_out1 = {
            let dtype = reshape28_out1.clone().dtype();
            self.layernormalization11
                .forward(reshape28_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear31_out1 = self.linear31.forward(layernormalization11_out1.clone());
        let linear32_out1 = self.linear32.forward(layernormalization11_out1.clone());
        let shape60_out1: [i64; 3] = {
            let axes = &linear32_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather53_out1 = shape60_out1[0] as i64;
        let gather54_out1 = shape60_out1[1] as i64;
        let unsqueeze59_out1 = [gather53_out1 as i64];
        let unsqueeze60_out1 = [gather54_out1 as i64];
        let constant555_out1: [i64; 1] = [64i64];
        let constant554_out1: [i64; 1] = [6i64];
        let concat31_out1: [i64; 4usize] = [
            &unsqueeze59_out1[..],
            &unsqueeze60_out1[..],
            &constant554_out1[..],
            &constant555_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape29_out1 = linear32_out1.reshape(concat31_out1);
        let linear33_out1 = self.linear33.forward(layernormalization11_out1);
        let shape62_out1: [i64; 3] = {
            let axes = &linear33_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather55_out1 = shape62_out1[0] as i64;
        let gather56_out1 = shape62_out1[1] as i64;
        let unsqueeze61_out1 = [gather55_out1 as i64];
        let unsqueeze62_out1 = [gather56_out1 as i64];
        let constant561_out1: [i64; 1] = [64i64];
        let constant560_out1: [i64; 1] = [6i64];
        let concat32_out1: [i64; 4usize] = [
            &unsqueeze61_out1[..],
            &unsqueeze62_out1[..],
            &constant560_out1[..],
            &constant561_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape30_out1 = linear33_out1.reshape(concat32_out1);
        let transpose23_out1 = reshape30_out1.permute([0, 2, 1, 3]);
        let shape64_out1: [i64; 3] = {
            let axes = &linear31_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather57_out1 = shape64_out1[0] as i64;
        let gather58_out1 = shape64_out1[1] as i64;
        let unsqueeze63_out1 = [gather57_out1 as i64];
        let unsqueeze64_out1 = [gather58_out1 as i64];
        let constant567_out1: [i64; 1] = [64i64];
        let constant566_out1: [i64; 1] = [6i64];
        let concat33_out1: [i64; 4usize] = [
            &unsqueeze63_out1[..],
            &unsqueeze64_out1[..],
            &constant566_out1[..],
            &constant567_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape31_out1 = linear31_out1.reshape(concat33_out1);
        let transpose24_out1 = reshape31_out1.permute([0, 2, 1, 3]);
        let transpose25_out1 = reshape29_out1.permute([0, 2, 3, 1]);
        let matmul44_k_corrected = transpose25_out1.permute([0, 1, 3, 2]);
        let (matmul45_out1,) = {
            let q = transpose24_out1;
            let k = matmul44_k_corrected;
            let v = transpose23_out1;
            let matmul45_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul45_out1,)
        };
        let transpose26_out1 = matmul45_out1.permute([0, 2, 1, 3]);
        let shape67_out1: [i64; 4] = {
            let axes = &transpose26_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather59_out1 = shape67_out1[0] as i64;
        let gather60_out1 = shape67_out1[1] as i64;
        let unsqueeze65_out1 = [gather59_out1 as i64];
        let unsqueeze66_out1 = [gather60_out1 as i64];
        let constant575_out1: [i64; 1] = [384i64];
        let concat34_out1: [i64; 3usize] = [
            &unsqueeze65_out1[..],
            &unsqueeze66_out1[..],
            &constant575_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape32_out1 = transpose26_out1.reshape(concat34_out1);
        let linear34_out1 = self.linear34.forward(reshape32_out1);
        let shape69_out1: [i64; 3] = {
            let axes = &reshape28_out1.dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather61_out1 = shape69_out1[0] as i64;
        let gather62_out1 = shape69_out1[1] as i64;
        let gather63_out1 = shape69_out1[2] as i64;
        let constant579_out1 = 16i64;
        let mul40_out1 = gather61_out1 * constant579_out1;
        let constant580_out1 = 16i64;
        let div19_out1 = gather62_out1 / constant580_out1;
        let unsqueeze67_out1 = [mul40_out1 as i64];
        let unsqueeze68_out1 = [div19_out1 as i64];
        let unsqueeze69_out1 = [gather63_out1 as i64];
        let concat35_out1: [i64; 3usize] = [
            &unsqueeze67_out1[..],
            &unsqueeze68_out1[..],
            &unsqueeze69_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape33_out1 = linear34_out1.reshape(concat35_out1);
        reshape33_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule7<B: Backend> {
    constant128: burn::module::Param<Tensor<B, 1>>,
    layernormalization12: LayerNorm<B>,
    linear35: Linear<B>,
    constant584: burn::module::Param<Tensor<B, 1>>,
    constant585: burn::module::Param<Tensor<B, 1>>,
    constant586: burn::module::Param<Tensor<B, 1>>,
    linear36: Linear<B>,
    constant133: burn::module::Param<Tensor<B, 1>>,
    layernormalization13: LayerNorm<B>,
    linear37: Linear<B>,
    linear38: Linear<B>,
    linear39: Linear<B>,
    linear40: Linear<B>,
    constant140: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule7<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant128: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant584: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant585: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant586: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant133: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization13 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear37 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear38 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear39 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear40 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant140: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            constant128,
            layernormalization12,
            linear35,
            constant584,
            constant585,
            constant586,
            linear36,
            constant133,
            layernormalization13,
            linear37,
            linear38,
            linear39,
            linear40,
            constant140,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        reshape33_out1: Tensor<B, 3>,
        add16_out1: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let constant128_out1 = self.constant128.val();
        let mul41_out1 = reshape33_out1
            .mul((constant128_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add17_out1 = mul41_out1.add(add16_out1);
        let layernormalization12_out1 = {
            let dtype = add17_out1.clone().dtype();
            self.layernormalization12
                .forward(add17_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear35_out1 = self.linear35.forward(layernormalization12_out1);
        let constant584_out1 = self.constant584.val();
        let div20_out1 = linear35_out1
            .clone()
            .div((constant584_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf6_out1 = div20_out1.erf();
        let constant585_out1 = self.constant585.val();
        let add18_out1 = erf6_out1
            .add((constant585_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul42_out1 = linear35_out1.mul(add18_out1);
        let constant586_out1 = self.constant586.val();
        let mul43_out1 = mul42_out1
            .mul((constant586_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear36_out1 = self.linear36.forward(mul43_out1);
        let constant133_out1 = self.constant133.val();
        let mul44_out1 = linear36_out1
            .mul((constant133_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add19_out1 = mul44_out1.add(add17_out1);
        let layernormalization13_out1 = {
            let dtype = add19_out1.clone().dtype();
            self.layernormalization13
                .forward(add19_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear37_out1 = self.linear37.forward(layernormalization13_out1.clone());
        let linear38_out1 = self.linear38.forward(layernormalization13_out1.clone());
        let shape72_out1: [i64; 3] = {
            let axes = &linear38_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather64_out1 = shape72_out1[0] as i64;
        let gather65_out1 = shape72_out1[1] as i64;
        let unsqueeze70_out1 = [gather64_out1 as i64];
        let unsqueeze71_out1 = [gather65_out1 as i64];
        let constant592_out1: [i64; 1] = [64i64];
        let constant591_out1: [i64; 1] = [6i64];
        let concat36_out1: [i64; 4usize] = [
            &unsqueeze70_out1[..],
            &unsqueeze71_out1[..],
            &constant591_out1[..],
            &constant592_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape34_out1 = linear38_out1.reshape(concat36_out1);
        let linear39_out1 = self.linear39.forward(layernormalization13_out1);
        let shape74_out1: [i64; 3] = {
            let axes = &linear39_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather66_out1 = shape74_out1[0] as i64;
        let gather67_out1 = shape74_out1[1] as i64;
        let unsqueeze72_out1 = [gather66_out1 as i64];
        let unsqueeze73_out1 = [gather67_out1 as i64];
        let constant598_out1: [i64; 1] = [64i64];
        let constant597_out1: [i64; 1] = [6i64];
        let concat37_out1: [i64; 4usize] = [
            &unsqueeze72_out1[..],
            &unsqueeze73_out1[..],
            &constant597_out1[..],
            &constant598_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape35_out1 = linear39_out1.reshape(concat37_out1);
        let transpose27_out1 = reshape35_out1.permute([0, 2, 1, 3]);
        let shape76_out1: [i64; 3] = {
            let axes = &linear37_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather68_out1 = shape76_out1[0] as i64;
        let gather69_out1 = shape76_out1[1] as i64;
        let unsqueeze74_out1 = [gather68_out1 as i64];
        let unsqueeze75_out1 = [gather69_out1 as i64];
        let constant604_out1: [i64; 1] = [64i64];
        let constant603_out1: [i64; 1] = [6i64];
        let concat38_out1: [i64; 4usize] = [
            &unsqueeze74_out1[..],
            &unsqueeze75_out1[..],
            &constant603_out1[..],
            &constant604_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape36_out1 = linear37_out1.reshape(concat38_out1);
        let transpose28_out1 = reshape36_out1.permute([0, 2, 1, 3]);
        let transpose29_out1 = reshape34_out1.permute([0, 2, 3, 1]);
        let matmul52_k_corrected = transpose29_out1.permute([0, 1, 3, 2]);
        let (matmul53_out1,) = {
            let q = transpose28_out1;
            let k = matmul52_k_corrected;
            let v = transpose27_out1;
            let matmul53_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul53_out1,)
        };
        let transpose30_out1 = matmul53_out1.permute([0, 2, 1, 3]);
        let shape79_out1: [i64; 4] = {
            let axes = &transpose30_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather70_out1 = shape79_out1[0] as i64;
        let gather71_out1 = shape79_out1[1] as i64;
        let unsqueeze76_out1 = [gather70_out1 as i64];
        let unsqueeze77_out1 = [gather71_out1 as i64];
        let constant612_out1: [i64; 1] = [384i64];
        let concat39_out1: [i64; 3usize] = [
            &unsqueeze76_out1[..],
            &unsqueeze77_out1[..],
            &constant612_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape37_out1 = transpose30_out1.reshape(concat39_out1);
        let linear40_out1 = self.linear40.forward(reshape37_out1);
        let constant140_out1 = self.constant140.val();
        let mul47_out1 = linear40_out1
            .mul((constant140_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add20_out1 = mul47_out1.add(add19_out1);
        add20_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule8<B: Backend> {
    layernormalization14: LayerNorm<B>,
    linear41: Linear<B>,
    constant613: burn::module::Param<Tensor<B, 1>>,
    constant614: burn::module::Param<Tensor<B, 1>>,
    constant615: burn::module::Param<Tensor<B, 1>>,
    linear42: Linear<B>,
    constant145: burn::module::Param<Tensor<B, 1>>,
    layernormalization15: LayerNorm<B>,
    linear43: Linear<B>,
    linear44: Linear<B>,
    linear45: Linear<B>,
    linear46: Linear<B>,
    constant152: burn::module::Param<Tensor<B, 1>>,
    layernormalization16: LayerNorm<B>,
    linear47: Linear<B>,
    constant642: burn::module::Param<Tensor<B, 1>>,
    constant643: burn::module::Param<Tensor<B, 1>>,
    constant644: burn::module::Param<Tensor<B, 1>>,
    linear48: Linear<B>,
    constant157: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule8<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization14 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear41 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant613: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant614: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant615: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant145: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear46 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant152: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization16 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear47 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant642: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant643: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant644: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant157: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            layernormalization14,
            linear41,
            constant613,
            constant614,
            constant615,
            linear42,
            constant145,
            layernormalization15,
            linear43,
            linear44,
            linear45,
            linear46,
            constant152,
            layernormalization16,
            linear47,
            constant642,
            constant643,
            constant644,
            linear48,
            constant157,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add20_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let layernormalization14_out1 = {
            let dtype = add20_out1.clone().dtype();
            self.layernormalization14
                .forward(add20_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear41_out1 = self.linear41.forward(layernormalization14_out1);
        let constant613_out1 = self.constant613.val();
        let div22_out1 = linear41_out1
            .clone()
            .div((constant613_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf7_out1 = div22_out1.erf();
        let constant614_out1 = self.constant614.val();
        let add21_out1 = erf7_out1
            .add((constant614_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul48_out1 = linear41_out1.mul(add21_out1);
        let constant615_out1 = self.constant615.val();
        let mul49_out1 = mul48_out1
            .mul((constant615_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear42_out1 = self.linear42.forward(mul49_out1);
        let constant145_out1 = self.constant145.val();
        let mul50_out1 = linear42_out1
            .mul((constant145_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add22_out1 = mul50_out1.add(add20_out1);
        let layernormalization15_out1 = {
            let dtype = add22_out1.clone().dtype();
            self.layernormalization15
                .forward(add22_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear43_out1 = self.linear43.forward(layernormalization15_out1.clone());
        let linear44_out1 = self.linear44.forward(layernormalization15_out1.clone());
        let shape81_out1: [i64; 3] = {
            let axes = &linear44_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather72_out1 = shape81_out1[0] as i64;
        let gather73_out1 = shape81_out1[1] as i64;
        let unsqueeze78_out1 = [gather72_out1 as i64];
        let unsqueeze79_out1 = [gather73_out1 as i64];
        let constant621_out1: [i64; 1] = [64i64];
        let constant620_out1: [i64; 1] = [6i64];
        let concat40_out1: [i64; 4usize] = [
            &unsqueeze78_out1[..],
            &unsqueeze79_out1[..],
            &constant620_out1[..],
            &constant621_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape38_out1 = linear44_out1.reshape(concat40_out1);
        let linear45_out1 = self.linear45.forward(layernormalization15_out1);
        let shape83_out1: [i64; 3] = {
            let axes = &linear45_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather74_out1 = shape83_out1[0] as i64;
        let gather75_out1 = shape83_out1[1] as i64;
        let unsqueeze80_out1 = [gather74_out1 as i64];
        let unsqueeze81_out1 = [gather75_out1 as i64];
        let constant627_out1: [i64; 1] = [64i64];
        let constant626_out1: [i64; 1] = [6i64];
        let concat41_out1: [i64; 4usize] = [
            &unsqueeze80_out1[..],
            &unsqueeze81_out1[..],
            &constant626_out1[..],
            &constant627_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape39_out1 = linear45_out1.reshape(concat41_out1);
        let transpose31_out1 = reshape39_out1.permute([0, 2, 1, 3]);
        let shape85_out1: [i64; 3] = {
            let axes = &linear43_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather76_out1 = shape85_out1[0] as i64;
        let gather77_out1 = shape85_out1[1] as i64;
        let unsqueeze82_out1 = [gather76_out1 as i64];
        let unsqueeze83_out1 = [gather77_out1 as i64];
        let constant633_out1: [i64; 1] = [64i64];
        let constant632_out1: [i64; 1] = [6i64];
        let concat42_out1: [i64; 4usize] = [
            &unsqueeze82_out1[..],
            &unsqueeze83_out1[..],
            &constant632_out1[..],
            &constant633_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape40_out1 = linear43_out1.reshape(concat42_out1);
        let transpose32_out1 = reshape40_out1.permute([0, 2, 1, 3]);
        let transpose33_out1 = reshape38_out1.permute([0, 2, 3, 1]);
        let matmul60_k_corrected = transpose33_out1.permute([0, 1, 3, 2]);
        let (matmul61_out1,) = {
            let q = transpose32_out1;
            let k = matmul60_k_corrected;
            let v = transpose31_out1;
            let matmul61_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul61_out1,)
        };
        let transpose34_out1 = matmul61_out1.permute([0, 2, 1, 3]);
        let shape88_out1: [i64; 4] = {
            let axes = &transpose34_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather78_out1 = shape88_out1[0] as i64;
        let gather79_out1 = shape88_out1[1] as i64;
        let unsqueeze84_out1 = [gather78_out1 as i64];
        let unsqueeze85_out1 = [gather79_out1 as i64];
        let constant641_out1: [i64; 1] = [384i64];
        let concat43_out1: [i64; 3usize] = [
            &unsqueeze84_out1[..],
            &unsqueeze85_out1[..],
            &constant641_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape41_out1 = transpose34_out1.reshape(concat43_out1);
        let linear46_out1 = self.linear46.forward(reshape41_out1);
        let constant152_out1 = self.constant152.val();
        let mul53_out1 = linear46_out1
            .mul((constant152_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add23_out1 = mul53_out1.add(add22_out1);
        let layernormalization16_out1 = {
            let dtype = add23_out1.clone().dtype();
            self.layernormalization16
                .forward(add23_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear47_out1 = self.linear47.forward(layernormalization16_out1);
        let constant642_out1 = self.constant642.val();
        let div24_out1 = linear47_out1
            .clone()
            .div((constant642_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf8_out1 = div24_out1.erf();
        let constant643_out1 = self.constant643.val();
        let add24_out1 = erf8_out1
            .add((constant643_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul54_out1 = linear47_out1.mul(add24_out1);
        let constant644_out1 = self.constant644.val();
        let mul55_out1 = mul54_out1
            .mul((constant644_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear48_out1 = self.linear48.forward(mul55_out1);
        let constant157_out1 = self.constant157.val();
        let mul56_out1 = linear48_out1
            .mul((constant157_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add25_out1 = mul56_out1.add(add23_out1);
        add25_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule9<B: Backend> {
    layernormalization17: LayerNorm<B>,
    linear49: Linear<B>,
    linear50: Linear<B>,
    linear51: Linear<B>,
    linear52: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule9<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization17 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear49 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear50 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear51 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear52 = LinearConfig::new(384, 384).with_bias(true).init(device);
        Self {
            layernormalization17,
            linear49,
            linear50,
            linear51,
            linear52,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, add25_out1: Tensor<B, 3>) -> Tensor<B, 3> {
        let shape90_out1: [i64; 3] = {
            let axes = &add25_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather80_out1 = shape90_out1[0] as i64;
        let gather81_out1 = shape90_out1[1] as i64;
        let gather82_out1 = shape90_out1[2] as i64;
        let constant648_out1 = 16i64;
        let div25_out1 = gather80_out1 / constant648_out1;
        let constant649_out1 = 16i64;
        let mul57_out1 = gather81_out1 * constant649_out1;
        let unsqueeze86_out1 = [div25_out1 as i64];
        let unsqueeze87_out1 = [mul57_out1 as i64];
        let unsqueeze88_out1 = [gather82_out1 as i64];
        let concat44_out1: [i64; 3usize] = [
            &unsqueeze86_out1[..],
            &unsqueeze87_out1[..],
            &unsqueeze88_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape42_out1 = add25_out1.reshape(concat44_out1);
        let layernormalization17_out1 = {
            let dtype = reshape42_out1.clone().dtype();
            self.layernormalization17
                .forward(reshape42_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear49_out1 = self.linear49.forward(layernormalization17_out1.clone());
        let linear50_out1 = self.linear50.forward(layernormalization17_out1.clone());
        let shape93_out1: [i64; 3] = {
            let axes = &linear50_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather83_out1 = shape93_out1[0] as i64;
        let gather84_out1 = shape93_out1[1] as i64;
        let unsqueeze89_out1 = [gather83_out1 as i64];
        let unsqueeze90_out1 = [gather84_out1 as i64];
        let constant658_out1: [i64; 1] = [64i64];
        let constant657_out1: [i64; 1] = [6i64];
        let concat45_out1: [i64; 4usize] = [
            &unsqueeze89_out1[..],
            &unsqueeze90_out1[..],
            &constant657_out1[..],
            &constant658_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape43_out1 = linear50_out1.reshape(concat45_out1);
        let linear51_out1 = self.linear51.forward(layernormalization17_out1);
        let shape95_out1: [i64; 3] = {
            let axes = &linear51_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather85_out1 = shape95_out1[0] as i64;
        let gather86_out1 = shape95_out1[1] as i64;
        let unsqueeze91_out1 = [gather85_out1 as i64];
        let unsqueeze92_out1 = [gather86_out1 as i64];
        let constant664_out1: [i64; 1] = [64i64];
        let constant663_out1: [i64; 1] = [6i64];
        let concat46_out1: [i64; 4usize] = [
            &unsqueeze91_out1[..],
            &unsqueeze92_out1[..],
            &constant663_out1[..],
            &constant664_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape44_out1 = linear51_out1.reshape(concat46_out1);
        let transpose35_out1 = reshape44_out1.permute([0, 2, 1, 3]);
        let shape97_out1: [i64; 3] = {
            let axes = &linear49_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather87_out1 = shape97_out1[0] as i64;
        let gather88_out1 = shape97_out1[1] as i64;
        let unsqueeze93_out1 = [gather87_out1 as i64];
        let unsqueeze94_out1 = [gather88_out1 as i64];
        let constant670_out1: [i64; 1] = [64i64];
        let constant669_out1: [i64; 1] = [6i64];
        let concat47_out1: [i64; 4usize] = [
            &unsqueeze93_out1[..],
            &unsqueeze94_out1[..],
            &constant669_out1[..],
            &constant670_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape45_out1 = linear49_out1.reshape(concat47_out1);
        let transpose36_out1 = reshape45_out1.permute([0, 2, 1, 3]);
        let transpose37_out1 = reshape43_out1.permute([0, 2, 3, 1]);
        let matmul68_k_corrected = transpose37_out1.permute([0, 1, 3, 2]);
        let (matmul69_out1,) = {
            let q = transpose36_out1;
            let k = matmul68_k_corrected;
            let v = transpose35_out1;
            let matmul69_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul69_out1,)
        };
        let transpose38_out1 = matmul69_out1.permute([0, 2, 1, 3]);
        let shape100_out1: [i64; 4] = {
            let axes = &transpose38_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather89_out1 = shape100_out1[0] as i64;
        let gather90_out1 = shape100_out1[1] as i64;
        let unsqueeze95_out1 = [gather89_out1 as i64];
        let unsqueeze96_out1 = [gather90_out1 as i64];
        let constant678_out1: [i64; 1] = [384i64];
        let concat48_out1: [i64; 3usize] = [
            &unsqueeze95_out1[..],
            &unsqueeze96_out1[..],
            &constant678_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape46_out1 = transpose38_out1.reshape(concat48_out1);
        let linear52_out1 = self.linear52.forward(reshape46_out1);
        let shape102_out1: [i64; 3] = {
            let axes = &reshape42_out1.dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather91_out1 = shape102_out1[0] as i64;
        let gather92_out1 = shape102_out1[1] as i64;
        let gather93_out1 = shape102_out1[2] as i64;
        let constant682_out1 = 16i64;
        let mul60_out1 = gather91_out1 * constant682_out1;
        let constant683_out1 = 16i64;
        let div27_out1 = gather92_out1 / constant683_out1;
        let unsqueeze97_out1 = [mul60_out1 as i64];
        let unsqueeze98_out1 = [div27_out1 as i64];
        let unsqueeze99_out1 = [gather93_out1 as i64];
        let concat49_out1: [i64; 3usize] = [
            &unsqueeze97_out1[..],
            &unsqueeze98_out1[..],
            &unsqueeze99_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape47_out1 = linear52_out1.reshape(concat49_out1);
        reshape47_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule10<B: Backend> {
    constant164: burn::module::Param<Tensor<B, 1>>,
    layernormalization18: LayerNorm<B>,
    linear53: Linear<B>,
    constant687: burn::module::Param<Tensor<B, 1>>,
    constant688: burn::module::Param<Tensor<B, 1>>,
    constant689: burn::module::Param<Tensor<B, 1>>,
    linear54: Linear<B>,
    constant169: burn::module::Param<Tensor<B, 1>>,
    layernormalization19: LayerNorm<B>,
    linear55: Linear<B>,
    linear56: Linear<B>,
    linear57: Linear<B>,
    linear58: Linear<B>,
    constant176: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule10<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant164: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant687: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant688: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant689: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant169: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let layernormalization19 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear55 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear56 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear57 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let linear58 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant176: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            constant164,
            layernormalization18,
            linear53,
            constant687,
            constant688,
            constant689,
            linear54,
            constant169,
            layernormalization19,
            linear55,
            linear56,
            linear57,
            linear58,
            constant176,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        reshape47_out1: Tensor<B, 3>,
        add25_out1: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let constant164_out1 = self.constant164.val();
        let mul61_out1 = reshape47_out1
            .mul((constant164_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add26_out1 = mul61_out1.add(add25_out1);
        let layernormalization18_out1 = {
            let dtype = add26_out1.clone().dtype();
            self.layernormalization18
                .forward(add26_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear53_out1 = self.linear53.forward(layernormalization18_out1);
        let constant687_out1 = self.constant687.val();
        let div28_out1 = linear53_out1
            .clone()
            .div((constant687_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf9_out1 = div28_out1.erf();
        let constant688_out1 = self.constant688.val();
        let add27_out1 = erf9_out1
            .add((constant688_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul62_out1 = linear53_out1.mul(add27_out1);
        let constant689_out1 = self.constant689.val();
        let mul63_out1 = mul62_out1
            .mul((constant689_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear54_out1 = self.linear54.forward(mul63_out1);
        let constant169_out1 = self.constant169.val();
        let mul64_out1 = linear54_out1
            .mul((constant169_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add28_out1 = mul64_out1.add(add26_out1);
        let layernormalization19_out1 = {
            let dtype = add28_out1.clone().dtype();
            self.layernormalization19
                .forward(add28_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear55_out1 = self.linear55.forward(layernormalization19_out1.clone());
        let linear56_out1 = self.linear56.forward(layernormalization19_out1.clone());
        let shape105_out1: [i64; 3] = {
            let axes = &linear56_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather94_out1 = shape105_out1[0] as i64;
        let gather95_out1 = shape105_out1[1] as i64;
        let unsqueeze100_out1 = [gather94_out1 as i64];
        let unsqueeze101_out1 = [gather95_out1 as i64];
        let constant695_out1: [i64; 1] = [64i64];
        let constant694_out1: [i64; 1] = [6i64];
        let concat50_out1: [i64; 4usize] = [
            &unsqueeze100_out1[..],
            &unsqueeze101_out1[..],
            &constant694_out1[..],
            &constant695_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape48_out1 = linear56_out1.reshape(concat50_out1);
        let linear57_out1 = self.linear57.forward(layernormalization19_out1);
        let shape107_out1: [i64; 3] = {
            let axes = &linear57_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather96_out1 = shape107_out1[0] as i64;
        let gather97_out1 = shape107_out1[1] as i64;
        let unsqueeze102_out1 = [gather96_out1 as i64];
        let unsqueeze103_out1 = [gather97_out1 as i64];
        let constant701_out1: [i64; 1] = [64i64];
        let constant700_out1: [i64; 1] = [6i64];
        let concat51_out1: [i64; 4usize] = [
            &unsqueeze102_out1[..],
            &unsqueeze103_out1[..],
            &constant700_out1[..],
            &constant701_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape49_out1 = linear57_out1.reshape(concat51_out1);
        let transpose39_out1 = reshape49_out1.permute([0, 2, 1, 3]);
        let shape109_out1: [i64; 3] = {
            let axes = &linear55_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather98_out1 = shape109_out1[0] as i64;
        let gather99_out1 = shape109_out1[1] as i64;
        let unsqueeze104_out1 = [gather98_out1 as i64];
        let unsqueeze105_out1 = [gather99_out1 as i64];
        let constant707_out1: [i64; 1] = [64i64];
        let constant706_out1: [i64; 1] = [6i64];
        let concat52_out1: [i64; 4usize] = [
            &unsqueeze104_out1[..],
            &unsqueeze105_out1[..],
            &constant706_out1[..],
            &constant707_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape50_out1 = linear55_out1.reshape(concat52_out1);
        let transpose40_out1 = reshape50_out1.permute([0, 2, 1, 3]);
        let transpose41_out1 = reshape48_out1.permute([0, 2, 3, 1]);
        let matmul76_k_corrected = transpose41_out1.permute([0, 1, 3, 2]);
        let (matmul77_out1,) = {
            let q = transpose40_out1;
            let k = matmul76_k_corrected;
            let v = transpose39_out1;
            let matmul77_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul77_out1,)
        };
        let transpose42_out1 = matmul77_out1.permute([0, 2, 1, 3]);
        let shape112_out1: [i64; 4] = {
            let axes = &transpose42_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather100_out1 = shape112_out1[0] as i64;
        let gather101_out1 = shape112_out1[1] as i64;
        let unsqueeze106_out1 = [gather100_out1 as i64];
        let unsqueeze107_out1 = [gather101_out1 as i64];
        let constant715_out1: [i64; 1] = [384i64];
        let concat53_out1: [i64; 3usize] = [
            &unsqueeze106_out1[..],
            &unsqueeze107_out1[..],
            &constant715_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape51_out1 = transpose42_out1.reshape(concat53_out1);
        let linear58_out1 = self.linear58.forward(reshape51_out1);
        let constant176_out1 = self.constant176.val();
        let mul67_out1 = linear58_out1
            .mul((constant176_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add29_out1 = mul67_out1.add(add28_out1);
        add29_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule11<B: Backend> {
    layernormalization20: LayerNorm<B>,
    linear59: Linear<B>,
    constant716: burn::module::Param<Tensor<B, 1>>,
    constant717: burn::module::Param<Tensor<B, 1>>,
    constant718: burn::module::Param<Tensor<B, 1>>,
    linear60: Linear<B>,
    constant181: burn::module::Param<Tensor<B, 1>>,
    layernormalization21: LayerNorm<B>,
    linear61: Linear<B>,
    linear62: Linear<B>,
    linear63: Linear<B>,
    linear64: Linear<B>,
    constant188: burn::module::Param<Tensor<B, 1>>,
    layernormalization22: LayerNorm<B>,
    linear65: Linear<B>,
    constant745: burn::module::Param<Tensor<B, 1>>,
    constant746: burn::module::Param<Tensor<B, 1>>,
    constant747: burn::module::Param<Tensor<B, 1>>,
    linear66: Linear<B>,
    constant193: burn::module::Param<Tensor<B, 1>>,
    layernormalization23: LayerNorm<B>,
    layernormalization24: LayerNorm<B>,
    layernormalization25: LayerNorm<B>,
    layernormalization26: LayerNorm<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule11<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization20 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear59 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant716: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant717: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant718: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant181: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear64 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant188: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant745: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant746: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant747: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant193: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let layernormalization24 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization25 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization26 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        Self {
            layernormalization20,
            linear59,
            constant716,
            constant717,
            constant718,
            linear60,
            constant181,
            layernormalization21,
            linear61,
            linear62,
            linear63,
            linear64,
            constant188,
            layernormalization22,
            linear65,
            constant745,
            constant746,
            constant747,
            linear66,
            constant193,
            layernormalization23,
            layernormalization24,
            layernormalization25,
            layernormalization26,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add29_out1: Tensor<B, 3>,
        add7_out1: Tensor<B, 3>,
        shape1_out1: [i64; 4],
        add16_out1: Tensor<B, 3>,
        add25_out1: Tensor<B, 3>,
    ) -> Tensor<B, 4> {
        let layernormalization20_out1 = {
            let dtype = add29_out1.clone().dtype();
            self.layernormalization20
                .forward(add29_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear59_out1 = self.linear59.forward(layernormalization20_out1);
        let constant716_out1 = self.constant716.val();
        let div30_out1 = linear59_out1
            .clone()
            .div((constant716_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf10_out1 = div30_out1.erf();
        let constant717_out1 = self.constant717.val();
        let add30_out1 = erf10_out1
            .add((constant717_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul68_out1 = linear59_out1.mul(add30_out1);
        let constant718_out1 = self.constant718.val();
        let mul69_out1 = mul68_out1
            .mul((constant718_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear60_out1 = self.linear60.forward(mul69_out1);
        let constant181_out1 = self.constant181.val();
        let mul70_out1 = linear60_out1
            .mul((constant181_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add31_out1 = mul70_out1.add(add29_out1);
        let layernormalization21_out1 = {
            let dtype = add31_out1.clone().dtype();
            self.layernormalization21
                .forward(add31_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear61_out1 = self.linear61.forward(layernormalization21_out1.clone());
        let linear62_out1 = self.linear62.forward(layernormalization21_out1.clone());
        let shape114_out1: [i64; 3] = {
            let axes = &linear62_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather102_out1 = shape114_out1[0] as i64;
        let gather103_out1 = shape114_out1[1] as i64;
        let unsqueeze108_out1 = [gather102_out1 as i64];
        let unsqueeze109_out1 = [gather103_out1 as i64];
        let constant724_out1: [i64; 1] = [64i64];
        let constant723_out1: [i64; 1] = [6i64];
        let concat54_out1: [i64; 4usize] = [
            &unsqueeze108_out1[..],
            &unsqueeze109_out1[..],
            &constant723_out1[..],
            &constant724_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape52_out1 = linear62_out1.reshape(concat54_out1);
        let linear63_out1 = self.linear63.forward(layernormalization21_out1);
        let shape116_out1: [i64; 3] = {
            let axes = &linear63_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather104_out1 = shape116_out1[0] as i64;
        let gather105_out1 = shape116_out1[1] as i64;
        let unsqueeze110_out1 = [gather104_out1 as i64];
        let unsqueeze111_out1 = [gather105_out1 as i64];
        let constant730_out1: [i64; 1] = [64i64];
        let constant729_out1: [i64; 1] = [6i64];
        let concat55_out1: [i64; 4usize] = [
            &unsqueeze110_out1[..],
            &unsqueeze111_out1[..],
            &constant729_out1[..],
            &constant730_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape53_out1 = linear63_out1.reshape(concat55_out1);
        let transpose43_out1 = reshape53_out1.permute([0, 2, 1, 3]);
        let shape118_out1: [i64; 3] = {
            let axes = &linear61_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather106_out1 = shape118_out1[0] as i64;
        let gather107_out1 = shape118_out1[1] as i64;
        let unsqueeze112_out1 = [gather106_out1 as i64];
        let unsqueeze113_out1 = [gather107_out1 as i64];
        let constant736_out1: [i64; 1] = [64i64];
        let constant735_out1: [i64; 1] = [6i64];
        let concat56_out1: [i64; 4usize] = [
            &unsqueeze112_out1[..],
            &unsqueeze113_out1[..],
            &constant735_out1[..],
            &constant736_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape54_out1 = linear61_out1.reshape(concat56_out1);
        let transpose44_out1 = reshape54_out1.permute([0, 2, 1, 3]);
        let transpose45_out1 = reshape52_out1.permute([0, 2, 3, 1]);
        let matmul84_k_corrected = transpose45_out1.permute([0, 1, 3, 2]);
        let (matmul85_out1,) = {
            let q = transpose44_out1;
            let k = matmul84_k_corrected;
            let v = transpose43_out1;
            let matmul85_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: None,
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul85_out1,)
        };
        let transpose46_out1 = matmul85_out1.permute([0, 2, 1, 3]);
        let shape121_out1: [i64; 4] = {
            let axes = &transpose46_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather108_out1 = shape121_out1[0] as i64;
        let gather109_out1 = shape121_out1[1] as i64;
        let unsqueeze114_out1 = [gather108_out1 as i64];
        let unsqueeze115_out1 = [gather109_out1 as i64];
        let constant744_out1: [i64; 1] = [384i64];
        let concat57_out1: [i64; 3usize] = [
            &unsqueeze114_out1[..],
            &unsqueeze115_out1[..],
            &constant744_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape55_out1 = transpose46_out1.reshape(concat57_out1);
        let linear64_out1 = self.linear64.forward(reshape55_out1);
        let constant188_out1 = self.constant188.val();
        let mul73_out1 = linear64_out1
            .mul((constant188_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add32_out1 = mul73_out1.add(add31_out1);
        let layernormalization22_out1 = {
            let dtype = add32_out1.clone().dtype();
            self.layernormalization22
                .forward(add32_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear65_out1 = self.linear65.forward(layernormalization22_out1);
        let constant745_out1 = self.constant745.val();
        let div32_out1 = linear65_out1
            .clone()
            .div((constant745_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf11_out1 = div32_out1.erf();
        let constant746_out1 = self.constant746.val();
        let add33_out1 = erf11_out1
            .add((constant746_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul74_out1 = linear65_out1.mul(add33_out1);
        let constant747_out1 = self.constant747.val();
        let mul75_out1 = mul74_out1
            .mul((constant747_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear66_out1 = self.linear66.forward(mul75_out1);
        let constant193_out1 = self.constant193.val();
        let mul76_out1 = linear66_out1
            .mul((constant193_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add34_out1 = mul76_out1.add(add32_out1);
        let layernormalization23_out1 = {
            let dtype = add7_out1.dtype();
            self.layernormalization23
                .forward(add7_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice15_out1 = layernormalization23_out1.slice(s![.., 1.., ..]);
        let gather110_out1 = shape1_out1[0] as i64;
        let gather111_out1 = shape1_out1[2] as i64;
        let gather112_out1 = shape1_out1[3] as i64;
        let constant755_out1 = 14i64;
        let div33_out1 = gather111_out1 / constant755_out1;
        let constant756_out1 = 14i64;
        let div34_out1 = gather112_out1 / constant756_out1;
        let shape126_out1: [i64; 3] = {
            let axes = &slice15_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather113_out1 = shape126_out1[0] as i64;
        let gather114_out1 = shape126_out1[1] as i64;
        let gather115_out1 = shape126_out1[2] as i64;
        let constant760_out1 = 4i64;
        let div35_out1 = div33_out1 / constant760_out1;
        let constant761_out1 = 4i64;
        let div36_out1 = div34_out1 / constant761_out1;
        let constant762_out1 = 16i64;
        let div37_out1 = gather113_out1 / constant762_out1;
        let constant763_out1 = 16i64;
        let mul77_out1 = gather114_out1 * constant763_out1;
        let unsqueeze116_out1 = [div37_out1 as i64];
        let unsqueeze117_out1 = [mul77_out1 as i64];
        let unsqueeze118_out1 = [gather115_out1 as i64];
        let concat58_out1: [i64; 3usize] = [
            &unsqueeze116_out1[..],
            &unsqueeze117_out1[..],
            &unsqueeze118_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape56_out1 = slice15_out1.reshape(concat58_out1);
        let constant767_out1 = 4i64;
        let mul78_out1 = div37_out1 * constant767_out1;
        let unsqueeze119_out1 = [mul78_out1 as i64];
        let unsqueeze120_out1 = [div35_out1 as i64];
        let unsqueeze121_out1 = [div36_out1 as i64];
        let unsqueeze122_out1 = [gather115_out1 as i64];
        let constant769_out1: [i64; 1] = [4i64];
        let concat59_out1: [i64; 5usize] = [
            &unsqueeze119_out1[..],
            &constant769_out1[..],
            &unsqueeze120_out1[..],
            &unsqueeze121_out1[..],
            &unsqueeze122_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape57_out1 = reshape56_out1.reshape(concat59_out1);
        let transpose47_out1 = reshape57_out1.permute([0, 2, 1, 3, 4]);
        let unsqueeze123_out1 = [gather110_out1 as i64];
        let unsqueeze124_out1 = [div33_out1 as i64];
        let unsqueeze125_out1 = [div34_out1 as i64];
        let constant776_out1: [i64; 1] = [-1i64];
        let concat60_out1: [i64; 4usize] = [
            &unsqueeze123_out1[..],
            &unsqueeze124_out1[..],
            &unsqueeze125_out1[..],
            &constant776_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape58_out1 = transpose47_out1.reshape(concat60_out1);
        let transpose48_out1 = reshape58_out1.permute([0, 3, 1, 2]);
        let layernormalization24_out1 = {
            let dtype = add16_out1.dtype();
            self.layernormalization24
                .forward(add16_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice16_out1 = layernormalization24_out1.slice(s![.., 1.., ..]);
        let gather116_out1 = shape1_out1[0] as i64;
        let gather117_out1 = shape1_out1[2] as i64;
        let gather118_out1 = shape1_out1[3] as i64;
        let constant784_out1 = 14i64;
        let div38_out1 = gather117_out1 / constant784_out1;
        let constant785_out1 = 14i64;
        let div39_out1 = gather118_out1 / constant785_out1;
        let shape132_out1: [i64; 3] = {
            let axes = &slice16_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather119_out1 = shape132_out1[0] as i64;
        let gather120_out1 = shape132_out1[1] as i64;
        let gather121_out1 = shape132_out1[2] as i64;
        let constant789_out1 = 4i64;
        let div40_out1 = div38_out1 / constant789_out1;
        let constant790_out1 = 4i64;
        let div41_out1 = div39_out1 / constant790_out1;
        let constant791_out1 = 16i64;
        let div42_out1 = gather119_out1 / constant791_out1;
        let constant792_out1 = 16i64;
        let mul79_out1 = gather120_out1 * constant792_out1;
        let unsqueeze126_out1 = [div42_out1 as i64];
        let unsqueeze127_out1 = [mul79_out1 as i64];
        let unsqueeze128_out1 = [gather121_out1 as i64];
        let concat61_out1: [i64; 3usize] = [
            &unsqueeze126_out1[..],
            &unsqueeze127_out1[..],
            &unsqueeze128_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape59_out1 = slice16_out1.reshape(concat61_out1);
        let constant796_out1 = 4i64;
        let mul80_out1 = div42_out1 * constant796_out1;
        let unsqueeze129_out1 = [mul80_out1 as i64];
        let unsqueeze130_out1 = [div40_out1 as i64];
        let unsqueeze131_out1 = [div41_out1 as i64];
        let unsqueeze132_out1 = [gather121_out1 as i64];
        let constant798_out1: [i64; 1] = [4i64];
        let concat62_out1: [i64; 5usize] = [
            &unsqueeze129_out1[..],
            &constant798_out1[..],
            &unsqueeze130_out1[..],
            &unsqueeze131_out1[..],
            &unsqueeze132_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape60_out1 = reshape59_out1.reshape(concat62_out1);
        let transpose49_out1 = reshape60_out1.permute([0, 2, 1, 3, 4]);
        let unsqueeze133_out1 = [gather116_out1 as i64];
        let unsqueeze134_out1 = [div38_out1 as i64];
        let unsqueeze135_out1 = [div39_out1 as i64];
        let constant805_out1: [i64; 1] = [-1i64];
        let concat63_out1: [i64; 4usize] = [
            &unsqueeze133_out1[..],
            &unsqueeze134_out1[..],
            &unsqueeze135_out1[..],
            &constant805_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape61_out1 = transpose49_out1.reshape(concat63_out1);
        let transpose50_out1 = reshape61_out1.permute([0, 3, 1, 2]);
        let layernormalization25_out1 = {
            let dtype = add25_out1.dtype();
            self.layernormalization25
                .forward(add25_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice17_out1 = layernormalization25_out1.slice(s![.., 1.., ..]);
        let gather122_out1 = shape1_out1[0] as i64;
        let gather123_out1 = shape1_out1[2] as i64;
        let gather124_out1 = shape1_out1[3] as i64;
        let constant813_out1 = 14i64;
        let div43_out1 = gather123_out1 / constant813_out1;
        let constant814_out1 = 14i64;
        let div44_out1 = gather124_out1 / constant814_out1;
        let shape138_out1: [i64; 3] = {
            let axes = &slice17_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather125_out1 = shape138_out1[0] as i64;
        let gather126_out1 = shape138_out1[1] as i64;
        let gather127_out1 = shape138_out1[2] as i64;
        let constant818_out1 = 4i64;
        let div45_out1 = div43_out1 / constant818_out1;
        let constant819_out1 = 4i64;
        let div46_out1 = div44_out1 / constant819_out1;
        let constant820_out1 = 16i64;
        let div47_out1 = gather125_out1 / constant820_out1;
        let constant821_out1 = 16i64;
        let mul81_out1 = gather126_out1 * constant821_out1;
        let unsqueeze136_out1 = [div47_out1 as i64];
        let unsqueeze137_out1 = [mul81_out1 as i64];
        let unsqueeze138_out1 = [gather127_out1 as i64];
        let concat64_out1: [i64; 3usize] = [
            &unsqueeze136_out1[..],
            &unsqueeze137_out1[..],
            &unsqueeze138_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape62_out1 = slice17_out1.reshape(concat64_out1);
        let constant825_out1 = 4i64;
        let mul82_out1 = div47_out1 * constant825_out1;
        let unsqueeze139_out1 = [mul82_out1 as i64];
        let unsqueeze140_out1 = [div45_out1 as i64];
        let unsqueeze141_out1 = [div46_out1 as i64];
        let unsqueeze142_out1 = [gather127_out1 as i64];
        let constant827_out1: [i64; 1] = [4i64];
        let concat65_out1: [i64; 5usize] = [
            &unsqueeze139_out1[..],
            &constant827_out1[..],
            &unsqueeze140_out1[..],
            &unsqueeze141_out1[..],
            &unsqueeze142_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape63_out1 = reshape62_out1.reshape(concat65_out1);
        let transpose51_out1 = reshape63_out1.permute([0, 2, 1, 3, 4]);
        let unsqueeze143_out1 = [gather122_out1 as i64];
        let unsqueeze144_out1 = [div43_out1 as i64];
        let unsqueeze145_out1 = [div44_out1 as i64];
        let constant834_out1: [i64; 1] = [-1i64];
        let concat66_out1: [i64; 4usize] = [
            &unsqueeze143_out1[..],
            &unsqueeze144_out1[..],
            &unsqueeze145_out1[..],
            &constant834_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape64_out1 = transpose51_out1.reshape(concat66_out1);
        let transpose52_out1 = reshape64_out1.permute([0, 3, 1, 2]);
        let layernormalization26_out1 = {
            let dtype = add34_out1.dtype();
            self.layernormalization26
                .forward(add34_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice18_out1 = layernormalization26_out1.slice(s![.., 1.., ..]);
        let gather128_out1 = shape1_out1[0] as i64;
        let gather129_out1 = shape1_out1[2] as i64;
        let gather130_out1 = shape1_out1[3] as i64;
        let constant842_out1 = 14i64;
        let div48_out1 = gather129_out1 / constant842_out1;
        let constant843_out1 = 14i64;
        let div49_out1 = gather130_out1 / constant843_out1;
        let shape144_out1: [i64; 3] = {
            let axes = &slice18_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather131_out1 = shape144_out1[0] as i64;
        let gather132_out1 = shape144_out1[1] as i64;
        let gather133_out1 = shape144_out1[2] as i64;
        let constant847_out1 = 4i64;
        let div50_out1 = div48_out1 / constant847_out1;
        let constant848_out1 = 4i64;
        let div51_out1 = div49_out1 / constant848_out1;
        let constant849_out1 = 16i64;
        let div52_out1 = gather131_out1 / constant849_out1;
        let constant850_out1 = 16i64;
        let mul83_out1 = gather132_out1 * constant850_out1;
        let unsqueeze146_out1 = [div52_out1 as i64];
        let unsqueeze147_out1 = [mul83_out1 as i64];
        let unsqueeze148_out1 = [gather133_out1 as i64];
        let concat67_out1: [i64; 3usize] = [
            &unsqueeze146_out1[..],
            &unsqueeze147_out1[..],
            &unsqueeze148_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape65_out1 = slice18_out1.reshape(concat67_out1);
        let constant854_out1 = 4i64;
        let mul84_out1 = div52_out1 * constant854_out1;
        let unsqueeze149_out1 = [mul84_out1 as i64];
        let unsqueeze150_out1 = [div50_out1 as i64];
        let unsqueeze151_out1 = [div51_out1 as i64];
        let unsqueeze152_out1 = [gather133_out1 as i64];
        let constant856_out1: [i64; 1] = [4i64];
        let concat68_out1: [i64; 5usize] = [
            &unsqueeze149_out1[..],
            &constant856_out1[..],
            &unsqueeze150_out1[..],
            &unsqueeze151_out1[..],
            &unsqueeze152_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape66_out1 = reshape65_out1.reshape(concat68_out1);
        let transpose53_out1 = reshape66_out1.permute([0, 2, 1, 3, 4]);
        let unsqueeze153_out1 = [gather128_out1 as i64];
        let unsqueeze154_out1 = [div48_out1 as i64];
        let unsqueeze155_out1 = [div49_out1 as i64];
        let constant863_out1: [i64; 1] = [-1i64];
        let concat69_out1: [i64; 4usize] = [
            &unsqueeze153_out1[..],
            &unsqueeze154_out1[..],
            &unsqueeze155_out1[..],
            &constant863_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape67_out1 = transpose53_out1.reshape(concat69_out1);
        let transpose54_out1 = reshape67_out1.permute([0, 3, 1, 2]);
        let concat70_out1 = burn::tensor::Tensor::cat(
            [transpose48_out1, transpose50_out1, transpose52_out1, transpose54_out1]
                .into(),
            1,
        );
        concat70_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule12<B: Backend> {
    conv2d2: Conv2d<B>,
    layernormalization27: LayerNorm<B>,
    conv2d3: Conv2d<B>,
    layernormalization28: LayerNorm<B>,
    conv2d4: Conv2d<B>,
    layernormalization29: LayerNorm<B>,
    conv2d5: Conv2d<B>,
    layernormalization30: LayerNorm<B>,
    conv2d6: Conv2d<B>,
    layernormalization31: LayerNorm<B>,
    conv2d7: Conv2d<B>,
    layernormalization32: LayerNorm<B>,
    conv2d8: Conv2d<B>,
    layernormalization33: LayerNorm<B>,
    conv2d9: Conv2d<B>,
    layernormalization34: LayerNorm<B>,
    layernormalization35: LayerNorm<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule12<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d2 = Conv2dConfig::new([1536, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization27 = LayerNormConfig::new(256)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d3 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization28 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d4 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization29 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d5 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization30 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d6 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization31 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d7 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization32 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d8 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization33 = LayerNormConfig::new(128)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let conv2d9 = Conv2dConfig::new([640, 256], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let layernormalization34 = LayerNormConfig::new(256)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let layernormalization35 = LayerNormConfig::new(256)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        Self {
            conv2d2,
            layernormalization27,
            conv2d3,
            layernormalization28,
            conv2d4,
            layernormalization29,
            conv2d5,
            layernormalization30,
            conv2d6,
            layernormalization31,
            conv2d7,
            layernormalization32,
            conv2d8,
            layernormalization33,
            conv2d9,
            layernormalization34,
            layernormalization35,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        concat70_out1: Tensor<B, 4>,
    ) -> ([i64; 1], i64, i64, Tensor<B, 3>) {
        let conv2d2_out1 = self.conv2d2.forward(concat70_out1);
        let transpose55_out1 = conv2d2_out1.permute([0, 2, 3, 1]);
        let layernormalization27_out1 = {
            let dtype = transpose55_out1.dtype();
            self.layernormalization27
                .forward(transpose55_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose56_out1 = layernormalization27_out1.permute([0, 3, 1, 2]);
        let sigmoid1_out1 = burn::tensor::activation::sigmoid(transpose56_out1.clone());
        let mul85_out1 = transpose56_out1.mul(sigmoid1_out1);
        let split_tensors = mul85_out1.split_with_sizes([128, 128].into(), 1);
        let [split1_out1, split1_out2] = split_tensors.try_into().unwrap();
        let conv2d3_out1 = self.conv2d3.forward(split1_out2.clone());
        let transpose57_out1 = conv2d3_out1.permute([0, 2, 3, 1]);
        let layernormalization28_out1 = {
            let dtype = transpose57_out1.dtype();
            self.layernormalization28
                .forward(transpose57_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose58_out1 = layernormalization28_out1.permute([0, 3, 1, 2]);
        let sigmoid2_out1 = burn::tensor::activation::sigmoid(transpose58_out1.clone());
        let mul86_out1 = transpose58_out1.mul(sigmoid2_out1);
        let conv2d4_out1 = self.conv2d4.forward(mul86_out1);
        let transpose59_out1 = conv2d4_out1.permute([0, 2, 3, 1]);
        let layernormalization29_out1 = {
            let dtype = transpose59_out1.dtype();
            self.layernormalization29
                .forward(transpose59_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose60_out1 = layernormalization29_out1.permute([0, 3, 1, 2]);
        let sigmoid3_out1 = burn::tensor::activation::sigmoid(transpose60_out1.clone());
        let mul87_out1 = transpose60_out1.mul(sigmoid3_out1);
        let conv2d5_out1 = self.conv2d5.forward(mul87_out1.clone());
        let transpose61_out1 = conv2d5_out1.permute([0, 2, 3, 1]);
        let layernormalization30_out1 = {
            let dtype = transpose61_out1.dtype();
            self.layernormalization30
                .forward(transpose61_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose62_out1 = layernormalization30_out1.permute([0, 3, 1, 2]);
        let sigmoid4_out1 = burn::tensor::activation::sigmoid(transpose62_out1.clone());
        let mul88_out1 = transpose62_out1.mul(sigmoid4_out1);
        let conv2d6_out1 = self.conv2d6.forward(mul88_out1);
        let transpose63_out1 = conv2d6_out1.permute([0, 2, 3, 1]);
        let layernormalization31_out1 = {
            let dtype = transpose63_out1.dtype();
            self.layernormalization31
                .forward(transpose63_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose64_out1 = layernormalization31_out1.permute([0, 3, 1, 2]);
        let sigmoid5_out1 = burn::tensor::activation::sigmoid(transpose64_out1.clone());
        let mul89_out1 = transpose64_out1.mul(sigmoid5_out1);
        let conv2d7_out1 = self.conv2d7.forward(mul89_out1.clone());
        let transpose65_out1 = conv2d7_out1.permute([0, 2, 3, 1]);
        let layernormalization32_out1 = {
            let dtype = transpose65_out1.dtype();
            self.layernormalization32
                .forward(transpose65_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose66_out1 = layernormalization32_out1.permute([0, 3, 1, 2]);
        let sigmoid6_out1 = burn::tensor::activation::sigmoid(transpose66_out1.clone());
        let mul90_out1 = transpose66_out1.mul(sigmoid6_out1);
        let conv2d8_out1 = self.conv2d8.forward(mul90_out1);
        let transpose67_out1 = conv2d8_out1.permute([0, 2, 3, 1]);
        let layernormalization33_out1 = {
            let dtype = transpose67_out1.dtype();
            self.layernormalization33
                .forward(transpose67_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose68_out1 = layernormalization33_out1.permute([0, 3, 1, 2]);
        let sigmoid7_out1 = burn::tensor::activation::sigmoid(transpose68_out1.clone());
        let mul91_out1 = transpose68_out1.mul(sigmoid7_out1);
        let concat71_out1 = burn::tensor::Tensor::cat(
            [split1_out1, split1_out2, mul87_out1, mul89_out1, mul91_out1].into(),
            1,
        );
        let conv2d9_out1 = self.conv2d9.forward(concat71_out1);
        let transpose69_out1 = conv2d9_out1.permute([0, 2, 3, 1]);
        let layernormalization34_out1 = {
            let dtype = transpose69_out1.dtype();
            self.layernormalization34
                .forward(transpose69_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose70_out1 = layernormalization34_out1.permute([0, 3, 1, 2]);
        let sigmoid8_out1 = burn::tensor::activation::sigmoid(transpose70_out1.clone());
        let mul92_out1 = transpose70_out1.mul(sigmoid8_out1);
        let transpose71_out1 = mul92_out1.permute([0, 2, 3, 1]);
        let layernormalization35_out1 = {
            let dtype = transpose71_out1.dtype();
            self.layernormalization35
                .forward(transpose71_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose72_out1 = layernormalization35_out1.permute([0, 3, 1, 2]);
        let shape147_out1: [i64; 4] = {
            let axes = &transpose72_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather134_out1 = shape147_out1[2] as i64;
        let gather135_out1 = shape147_out1[3] as i64;
        let slice19_out1: [i64; 2] = shape147_out1[0..2].try_into().unwrap();
        let constant870_out1: [i64; 1] = [-1i64];
        let concat72_out1: [i64; 3usize] = [&slice19_out1[..], &constant870_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape68_out1 = transpose72_out1.reshape(concat72_out1);
        let transpose73_out1 = reshape68_out1.permute([0, 2, 1]);
        let gather136_out1 = {
            let sliced = transpose73_out1.clone().slice(s![.., 0, ..]);
            sliced.squeeze_dim::<2usize>(1)
        };
        let gather137_out1 = {
            let sliced = gather136_out1.slice(s![.., 0]);
            sliced.squeeze_dim::<1usize>(1)
        };
        let shape150_out1: [i64; 1] = {
            let axes = &gather137_out1.dims()[0..1];
            let mut output = [0i64; 1];
            for i in 0..1 {
                output[i] = axes[i] as i64;
            }
            output
        };
        (shape150_out1, gather134_out1, gather135_out1, transpose73_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule13<B: Backend> {
    constant893: burn::module::Param<Tensor<B, 1>>,
    constant894: burn::module::Param<Tensor<B, 1>>,
    linear67: Linear<B>,
    layernormalization36: LayerNorm<B>,
    linear68: Linear<B>,
    linear69: Linear<B>,
    linear70: Linear<B>,
    linear71: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule13<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant893: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant894: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.05000000074505806f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear67 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization36 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear68 = LinearConfig::new(256, 18).with_bias(true).init(device);
        let linear69 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear70 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear71 = LinearConfig::new(256, 4).with_bias(true).init(device);
        Self {
            constant893,
            constant894,
            linear67,
            layernormalization36,
            linear68,
            linear69,
            linear70,
            linear71,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        shape150_out1: [i64; 1],
        gather134_out1: i64,
        gather135_out1: i64,
        transpose73_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, i64) {
        let constantofshape3_out1 = Tensor::<
            B,
            1,
        >::from_data(
                burn::tensor::TensorData::from([0f32 as f64]),
                (&self.device, burn::tensor::DType::F32),
            )
            .reshape([1])
            .expand(shape150_out1);
        let cast84_out1 = constantofshape3_out1.int().cast(burn::tensor::DType::I64);
        let add35_out1 = cast84_out1.clone().add_scalar(gather134_out1);
        let add36_out1 = cast84_out1.add_scalar(gather135_out1);
        let constant871_out1 = 1i64;
        let sub1_out1 = gather134_out1 - constant871_out1;
        let range1_out1 = {
            let __start = 0i64;
            let __limit = gather134_out1;
            let __delta = 1i64;
            assert!(__delta != 0);
            let __n = ((__limit - __start) as f64 / __delta as f64).ceil().max(0.0)
                as i64;
            Tensor::arange(0..__n, &self.device)
                .cast(burn::tensor::DType::I64)
                .mul_scalar(__delta)
                .add_scalar(__start)
        };
        let constant875_out1 = 1i64;
        let sub3_out1 = gather134_out1 - constant875_out1;
        let cast86_out1 = sub1_out1 as f32;
        let cast87_out1 = sub3_out1 as f32;
        let div53_out1 = cast86_out1 / cast87_out1;
        let cast88_out1 = range1_out1.float().cast(burn::tensor::DType::F32);
        let mul93_out1 = cast88_out1.mul_scalar(div53_out1);
        let constant877_out1 = 1i64;
        let sub4_out1 = gather135_out1 - constant877_out1;
        let range2_out1 = {
            let __start = 0i64;
            let __limit = gather135_out1;
            let __delta = 1i64;
            assert!(__delta != 0);
            let __n = ((__limit - __start) as f64 / __delta as f64).ceil().max(0.0)
                as i64;
            Tensor::arange(0..__n, &self.device)
                .cast(burn::tensor::DType::I64)
                .mul_scalar(__delta)
                .add_scalar(__start)
        };
        let constant881_out1 = 1i64;
        let sub6_out1 = gather135_out1 - constant881_out1;
        let cast90_out1 = sub4_out1 as f32;
        let cast91_out1 = sub6_out1 as f32;
        let div54_out1 = cast90_out1 / cast91_out1;
        let cast92_out1 = range2_out1.float().cast(burn::tensor::DType::F32);
        let mul94_out1 = cast92_out1.mul_scalar(div54_out1);
        let reshape69_out1 = mul93_out1.reshape([-1]);
        let reshape70_out1 = mul94_out1.reshape([-1]);
        let shape151_out1: [i64; 1] = {
            let axes = &reshape69_out1.clone().dims()[0..1];
            let mut output = [0i64; 1];
            for i in 0..1 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let shape152_out1: [i64; 1] = {
            let axes = &reshape70_out1.clone().dims()[0..1];
            let mut output = [0i64; 1];
            for i in 0..1 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let concat74_out1: [i64; 2usize] = [&shape151_out1[..], &shape152_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let constant885_out1: [i64; 1] = [1i64];
        let concat75_out1: [i64; 2usize] = [&shape151_out1[..], &constant885_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape71_out1 = reshape69_out1.reshape(concat75_out1);
        let expand3_out1 = {
            let onnx_shape: [i64; 2usize] = concat74_out1;
            let input_dims = reshape71_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..2usize {
                let dim_offset = 2usize - 2usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            reshape71_out1.expand(shape)
        };
        let constant886_out1: [i64; 1] = [1i64];
        let concat76_out1: [i64; 2usize] = [&constant886_out1[..], &shape152_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape72_out1 = reshape70_out1.reshape(concat76_out1);
        let expand4_out1 = {
            let onnx_shape: [i64; 2usize] = concat74_out1;
            let input_dims = reshape72_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..2usize {
                let dim_offset = 2usize - 2usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            reshape72_out1.expand(shape)
        };
        let unsqueeze156_out1: Tensor<B, 3> = expand4_out1.unsqueeze_dims::<3>(&[-1]);
        let unsqueeze157_out1: Tensor<B, 3> = expand3_out1.unsqueeze_dims::<3>(&[-1]);
        let concat77_out1 = burn::tensor::Tensor::cat(
            [unsqueeze156_out1, unsqueeze157_out1].into(),
            2,
        );
        let unsqueeze158_out1: Tensor<B, 2, Int> = add36_out1.unsqueeze_dims::<2>(&[-1]);
        let unsqueeze159_out1: Tensor<B, 2, Int> = add35_out1.unsqueeze_dims::<2>(&[-1]);
        let concat78_out1 = burn::tensor::Tensor::cat(
            [unsqueeze158_out1, unsqueeze159_out1].into(),
            1,
        );
        let reshape73_out1 = concat78_out1.reshape([-1, 1, 1, 2]);
        let unsqueeze160_out1: Tensor<B, 4> = concat77_out1.unsqueeze_dims::<4>(&[0]);
        let constant893_out1 = self.constant893.val();
        let add39_out1 = unsqueeze160_out1
            .add((constant893_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let cast93_out1 = reshape73_out1.float().cast(burn::tensor::DType::F32);
        let div55_out1 = add39_out1.div(cast93_out1);
        let shape153_out1: [i64; 4] = {
            let axes = &div55_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let constantofshape4_out1 = Tensor::<
            B,
            1,
        >::from_data(
                burn::tensor::TensorData::from([1f32 as f64]),
                (&self.device, burn::tensor::DType::F32),
            )
            .reshape([1, 1, 1, 1])
            .expand(shape153_out1);
        let constant894_out1 = self.constant894.val();
        let mul95_out1 = constantofshape4_out1
            .mul((constant894_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat79_out1 = burn::tensor::Tensor::cat(
            [div55_out1, mul95_out1].into(),
            3,
        );
        let mul97_out1 = gather134_out1 * gather135_out1;
        let unsqueeze161_out1 = [mul97_out1 as i64];
        let constant898_out1: [i64; 1] = [4i64];
        let constant896_out1: [i64; 1] = [-1i64];
        let concat80_out1: [i64; 3usize] = [
            &constant896_out1[..],
            &unsqueeze161_out1[..],
            &constant898_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape74_out1 = concat79_out1.reshape(concat80_out1);
        let constant899_out1 = 0.01f32;
        let greater1_out1 = reshape74_out1.clone().greater_elem(constant899_out1);
        let constant900_out1 = 0.99f32;
        let less1_out1 = reshape74_out1.clone().lower_elem(constant900_out1);
        let and1_out1 = greater1_out1.bool_and(less1_out1);
        let not1_out1 = and1_out1.bool_not();
        let cast94_out1 = not1_out1.int().cast(burn::tensor::DType::I64);
        let reducesum1_out1 = { cast94_out1.sum_dim(2usize) };
        let constant902_out1 = 0i64;
        let greater2_out1 = reducesum1_out1.greater_elem(constant902_out1);
        let not2_out1 = greater2_out1.bool_not();
        let not3_out1 = not2_out1.bool_not();
        let constant903_out1 = 0f32;
        let where2_out1 = reshape74_out1.mask_fill(not3_out1.clone(), constant903_out1);
        let constant904_out1 = 0f32;
        let where3_out1 = transpose73_out1.mask_fill(not3_out1, constant904_out1);
        let linear67_out1 = self.linear67.forward(where3_out1);
        let layernormalization36_out1 = {
            let dtype = linear67_out1.dtype();
            self.layernormalization36
                .forward(linear67_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear68_out1 = self.linear68.forward(layernormalization36_out1.clone());
        let linear69_out1 = self.linear69.forward(layernormalization36_out1);
        let relu1_out1 = burn::tensor::activation::relu(linear69_out1);
        let linear70_out1 = self.linear70.forward(relu1_out1);
        let relu2_out1 = burn::tensor::activation::relu(linear70_out1);
        let linear71_out1 = self.linear71.forward(relu2_out1);
        let slice20_out1 = linear71_out1.clone().slice(s![.., .., 0..2]);
        let slice21_out1 = where2_out1.clone().slice(s![.., .., 2..]);
        let mul98_out1 = slice20_out1.mul(slice21_out1.clone());
        let slice22_out1 = where2_out1.slice(s![.., .., 0..2]);
        let add40_out1 = mul98_out1.add(slice22_out1);
        let slice23_out1 = linear71_out1.slice(s![.., .., 2..]);
        let exp1_out1 = slice23_out1.exp();
        let mul99_out1 = exp1_out1.mul(slice21_out1);
        let concat82_out1 = burn::tensor::Tensor::cat(
            [add40_out1, mul99_out1].into(),
            2,
        );
        let reducemax1_out1 = {
            linear68_out1.max_dim(2usize).squeeze_dims::<2usize>(&[2])
        };
        let (topk1_out1, __topk_indices_raw) = reducemax1_out1.topk_with_indices(300, 1);
        let topk1_out2 = __topk_indices_raw.cast(burn::tensor::DType::I64);
        let unsqueeze162_out1: Tensor<B, 3, Int> = topk1_out2.unsqueeze_dims::<3>(&[-1]);
        let constantofshape5_out1: [i64; 3usize] = [1i64, 1i64, 1i64];
        let expand5_out1 = {
            let onnx_shape: [i64; 3usize] = constantofshape5_out1;
            let input_dims = unsqueeze162_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            unsqueeze162_out1.expand(shape)
        };
        let tile2_out1 = expand5_out1.repeat(&[1, 1, 4]);
        let gatherelements1_out1 = concat82_out1.gather(1, tile2_out1);
        (gatherelements1_out1, mul97_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule14<B: Backend> {
    constant930: burn::module::Param<Tensor<B, 1, Int>>,
    constant293: burn::module::Param<Tensor<B, 3>>,
    constant935: burn::module::Param<Tensor<B, 1, Int>>,
    constant294: burn::module::Param<Tensor<B, 3>>,
    constant965: burn::module::Param<Tensor<B, 1>>,
    constant966: burn::module::Param<Tensor<B, 1>>,
    constant968: burn::module::Param<Tensor<B, 1>>,
    constant970: burn::module::Param<Tensor<B, 1>>,
    constant999: burn::module::Param<Tensor<B, 1>>,
    constant1001: burn::module::Param<Tensor<B, 1>>,
    constant1016: burn::module::Param<Tensor<B, 1>>,
    constant1018: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule14<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant930: burn::module::Param<Tensor<B, 1, Int>> = burn::module::Param::uninitialized(
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
        let constant293: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 300, 256], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 300, 256].into(),
        );
        let constant935: burn::module::Param<Tensor<B, 1, Int>> = burn::module::Param::uninitialized(
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
        let constant294: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 300, 4], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 300, 4].into(),
        );
        let constant965: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([6.2831854820251465f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant966: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([6.2831854820251465f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant968: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([128], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [128].into(),
        );
        let constant970: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([128], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [128].into(),
        );
        let constant999: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([6.2831854820251465f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1001: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([128], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [128].into(),
        );
        let constant1016: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([6.2831854820251465f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1018: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([128], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [128].into(),
        );
        Self {
            constant930,
            constant293,
            constant935,
            constant294,
            constant965,
            constant966,
            constant968,
            constant970,
            constant999,
            constant1001,
            constant1016,
            constant1018,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        transpose73_out1: Tensor<B, 3>,
        gatherelements1_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>, [i64; 3], i64, Tensor<B, 3>) {
        let shape154_out1: [i64; 3] = {
            let axes = &transpose73_out1.dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather138_out1 = shape154_out1[0] as i64;
        let unsqueeze163_out1 = [gather138_out1 as i64];
        let constant928_out1: [i64; 1] = [-1i64];
        let constant927_out1: [i64; 1] = [-1i64];
        let concat84_out1: [i64; 3usize] = [
            &unsqueeze163_out1[..],
            &constant927_out1[..],
            &constant928_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape75_out1 = concat84_out1;
        let shape155_out1: [i64; 1] = [3i64];
        let constantofshape6_out1 = Tensor::<
            B,
            1,
            Int,
        >::from_data(
                burn::tensor::TensorData::from([1i64 as i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .reshape([1])
            .expand(shape155_out1);
        let constant930_out1 = self.constant930.val();
        let mul100_out1 = constantofshape6_out1.clone().mul(constant930_out1);
        let equal2_out1 = {
            let shape_tensor = Tensor::<
                B,
                1,
                Int,
            >::from_data(
                burn::tensor::TensorData::from(reshape75_out1.as_slice()),
                (&self.device, burn::tensor::DType::I64),
            );
            shape_tensor.equal(mul100_out1)
        };
        let where4_out1 = Tensor::<
            B,
            1,
            burn::tensor::Int,
        >::from_data(
                burn::tensor::TensorData::from(&reshape75_out1 as &[i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .mask_where(equal2_out1, constantofshape6_out1);
        let constant293_out1 = self.constant293.val();
        let expand6_out1 = {
            let onnx_shape: [i64; 3usize] = TryInto::<
                [i64; 3usize],
            >::try_into(where4_out1.to_data().convert::<i64>().as_slice().unwrap())
                .unwrap();
            let input_dims = constant293_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            constant293_out1.expand(shape)
        };
        let unsqueeze164_out1 = [gather138_out1 as i64];
        let constant933_out1: [i64; 1] = [-1i64];
        let constant932_out1: [i64; 1] = [-1i64];
        let concat85_out1: [i64; 3usize] = [
            &unsqueeze164_out1[..],
            &constant932_out1[..],
            &constant933_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape76_out1 = concat85_out1;
        let shape156_out1: [i64; 1] = [3i64];
        let constantofshape7_out1 = Tensor::<
            B,
            1,
            Int,
        >::from_data(
                burn::tensor::TensorData::from([1i64 as i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .reshape([1])
            .expand(shape156_out1);
        let constant935_out1 = self.constant935.val();
        let mul101_out1 = constantofshape7_out1.clone().mul(constant935_out1);
        let equal3_out1 = {
            let shape_tensor = Tensor::<
                B,
                1,
                Int,
            >::from_data(
                burn::tensor::TensorData::from(reshape76_out1.as_slice()),
                (&self.device, burn::tensor::DType::I64),
            );
            shape_tensor.equal(mul101_out1)
        };
        let where5_out1 = Tensor::<
            B,
            1,
            burn::tensor::Int,
        >::from_data(
                burn::tensor::TensorData::from(&reshape76_out1 as &[i64]),
                (&self.device, burn::tensor::DType::I64),
            )
            .mask_where(equal3_out1, constantofshape7_out1);
        let constant294_out1 = self.constant294.val();
        let expand7_out1 = {
            let onnx_shape: [i64; 3usize] = TryInto::<
                [i64; 3usize],
            >::try_into(where5_out1.to_data().convert::<i64>().as_slice().unwrap())
                .unwrap();
            let input_dims = constant294_out1.dims();
            let mut shape = onnx_shape;
            #[allow(clippy::needless_range_loop)]
            for i in 0..3usize {
                let dim_offset = 3usize - 3usize + i;
                if shape[dim_offset] == 1 && input_dims[i] > 1 {
                    shape[dim_offset] = input_dims[i] as i64;
                }
            }
            constant294_out1.expand(shape)
        };
        let shape157_out1: [i64; 3] = {
            let axes = &gatherelements1_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather139_out1 = shape157_out1[1] as i64;
        let unsqueeze165_out1 = [gather139_out1 as i64];
        let slice24_out1 = expand7_out1
            .clone()
            .slice(s![.., 0..unsqueeze165_out1[0], ..]);
        let unsqueeze166_out1 = [gather139_out1 as i64];
        let slice25_out1 = expand7_out1
            .slice(s![.., unsqueeze166_out1[0]..9223372036854775807, ..]);
        let slice26_out1 = slice24_out1.clone().slice(s![.., .., 0..2]);
        let slice27_out1 = gatherelements1_out1.clone().slice(s![.., .., 2..]);
        let mul102_out1 = slice26_out1.mul(slice27_out1.clone());
        let slice28_out1 = gatherelements1_out1.slice(s![.., .., 0..2]);
        let add41_out1 = mul102_out1.add(slice28_out1);
        let slice29_out1 = slice24_out1.slice(s![.., .., 2..]);
        let exp2_out1 = slice29_out1.exp();
        let mul103_out1 = exp2_out1.mul(slice27_out1);
        let concat86_out1 = burn::tensor::Tensor::cat(
            [add41_out1, mul103_out1].into(),
            2,
        );
        let concat87_out1 = burn::tensor::Tensor::cat(
            [concat86_out1, slice25_out1].into(),
            1,
        );
        let slice30_out1 = concat87_out1.clone().slice(s![.., .., 0..4]);
        let gather140_out1 = {
            let sliced = slice30_out1.clone().slice(s![.., .., 0]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let constant965_out1 = self.constant965.val();
        let mul104_out1 = gather140_out1
            .mul((constant965_out1).unsqueeze_dims(&[0isize]));
        let gather141_out1 = {
            let sliced = slice30_out1.clone().slice(s![.., .., 1]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let constant966_out1 = self.constant966.val();
        let mul105_out1 = gather141_out1
            .mul((constant966_out1).unsqueeze_dims(&[0isize]));
        let unsqueeze167_out1: Tensor<B, 3> = mul104_out1.unsqueeze_dims::<3>(&[2]);
        let constant968_out1 = self.constant968.val();
        let div56_out1 = unsqueeze167_out1
            .div((constant968_out1).unsqueeze_dims(&[0isize, 1isize]));
        let unsqueeze168_out1: Tensor<B, 3> = mul105_out1.unsqueeze_dims::<3>(&[2]);
        let constant970_out1 = self.constant970.val();
        let div57_out1 = unsqueeze168_out1
            .div((constant970_out1).unsqueeze_dims(&[0isize, 1isize]));
        let slice31_out1 = div56_out1.clone().slice(s![.., .., 0..; 2]);
        let sin1_out1 = slice31_out1.sin();
        let slice32_out1 = div56_out1.slice(s![.., .., 1..; 2]);
        let cos1_out1 = slice32_out1.cos();
        let unsqueeze169_out1: Tensor<B, 4> = sin1_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze170_out1: Tensor<B, 4> = cos1_out1.unsqueeze_dims::<4>(&[3]);
        let concat88_out1 = burn::tensor::Tensor::cat(
            [unsqueeze169_out1, unsqueeze170_out1].into(),
            3,
        );
        let shape158_out1: [i64; 4] = {
            let axes = &concat88_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice33_out1: [i64; 2] = shape158_out1[0..2].try_into().unwrap();
        let constant984_out1: [i64; 1] = [-1i64];
        let concat89_out1: [i64; 3usize] = [&slice33_out1[..], &constant984_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape77_out1 = concat88_out1.reshape(concat89_out1);
        let slice34_out1 = div57_out1.clone().slice(s![.., .., 0..; 2]);
        let sin2_out1 = slice34_out1.sin();
        let slice35_out1 = div57_out1.slice(s![.., .., 1..; 2]);
        let cos2_out1 = slice35_out1.cos();
        let unsqueeze171_out1: Tensor<B, 4> = sin2_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze172_out1: Tensor<B, 4> = cos2_out1.unsqueeze_dims::<4>(&[3]);
        let concat90_out1 = burn::tensor::Tensor::cat(
            [unsqueeze171_out1, unsqueeze172_out1].into(),
            3,
        );
        let shape159_out1: [i64; 4] = {
            let axes = &concat90_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice36_out1: [i64; 2] = shape159_out1[0..2].try_into().unwrap();
        let constant998_out1: [i64; 1] = [-1i64];
        let concat91_out1: [i64; 3usize] = [&slice36_out1[..], &constant998_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape78_out1 = concat90_out1.reshape(concat91_out1);
        let gather142_out1 = {
            let sliced = slice30_out1.clone().slice(s![.., .., 2]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let constant999_out1 = self.constant999.val();
        let mul106_out1 = gather142_out1
            .mul((constant999_out1).unsqueeze_dims(&[0isize]));
        let unsqueeze173_out1: Tensor<B, 3> = mul106_out1.unsqueeze_dims::<3>(&[2]);
        let constant1001_out1 = self.constant1001.val();
        let div58_out1 = unsqueeze173_out1
            .div((constant1001_out1).unsqueeze_dims(&[0isize, 1isize]));
        let slice37_out1 = div58_out1.clone().slice(s![.., .., 0..; 2]);
        let sin3_out1 = slice37_out1.sin();
        let slice38_out1 = div58_out1.slice(s![.., .., 1..; 2]);
        let cos3_out1 = slice38_out1.cos();
        let unsqueeze174_out1: Tensor<B, 4> = sin3_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze175_out1: Tensor<B, 4> = cos3_out1.unsqueeze_dims::<4>(&[3]);
        let concat92_out1 = burn::tensor::Tensor::cat(
            [unsqueeze174_out1, unsqueeze175_out1].into(),
            3,
        );
        let shape160_out1: [i64; 4] = {
            let axes = &concat92_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice39_out1: [i64; 2] = shape160_out1[0..2].try_into().unwrap();
        let constant1015_out1: [i64; 1] = [-1i64];
        let concat93_out1: [i64; 3usize] = [&slice39_out1[..], &constant1015_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape79_out1 = concat92_out1.reshape(concat93_out1);
        let gather143_out1 = {
            let sliced = slice30_out1.clone().slice(s![.., .., 3]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let constant1016_out1 = self.constant1016.val();
        let mul107_out1 = gather143_out1
            .mul((constant1016_out1).unsqueeze_dims(&[0isize]));
        let unsqueeze176_out1: Tensor<B, 3> = mul107_out1.unsqueeze_dims::<3>(&[2]);
        let constant1018_out1 = self.constant1018.val();
        let div59_out1 = unsqueeze176_out1
            .div((constant1018_out1).unsqueeze_dims(&[0isize, 1isize]));
        let slice40_out1 = div59_out1.clone().slice(s![.., .., 0..; 2]);
        let sin4_out1 = slice40_out1.sin();
        let slice41_out1 = div59_out1.slice(s![.., .., 1..; 2]);
        let cos4_out1 = slice41_out1.cos();
        let unsqueeze177_out1: Tensor<B, 4> = sin4_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze178_out1: Tensor<B, 4> = cos4_out1.unsqueeze_dims::<4>(&[3]);
        let concat94_out1 = burn::tensor::Tensor::cat(
            [unsqueeze177_out1, unsqueeze178_out1].into(),
            3,
        );
        let shape161_out1: [i64; 4] = {
            let axes = &concat94_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice42_out1: [i64; 2] = shape161_out1[0..2].try_into().unwrap();
        let constant1032_out1: [i64; 1] = [-1i64];
        let concat95_out1: [i64; 3usize] = [&slice42_out1[..], &constant1032_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape80_out1 = concat94_out1.reshape(concat95_out1);
        let concat96_out1 = burn::tensor::Tensor::cat(
            [reshape78_out1, reshape77_out1, reshape79_out1, reshape80_out1].into(),
            2,
        );
        (
            slice30_out1,
            concat96_out1,
            expand6_out1,
            shape154_out1,
            gather138_out1,
            concat87_out1,
        )
    }
}
#[derive(Module, Debug)]
pub struct Submodule15<B: Backend> {
    linear72: Linear<B>,
    linear73: Linear<B>,
    linear74: Linear<B>,
    linear75: Linear<B>,
    linear76: Linear<B>,
    constant1065: burn::module::Param<Tensor<B, 1>>,
    linear77: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule15<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let linear72 = LinearConfig::new(512, 256).with_bias(true).init(device);
        let linear73 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear74 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear75 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear76 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let constant1065: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([1], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1].into(),
        );
        let linear77 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        Self {
            linear72,
            linear73,
            linear74,
            linear75,
            linear76,
            constant1065,
            linear77,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        slice30_out1: Tensor<B, 3>,
        concat96_out1: Tensor<B, 3>,
        expand6_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 4>) {
        let unsqueeze179_out1: Tensor<B, 4> = slice30_out1.unsqueeze_dims::<4>(&[2]);
        let linear72_out1 = self.linear72.forward(concat96_out1);
        let relu3_out1 = burn::tensor::activation::relu(linear72_out1);
        let linear73_out1 = self.linear73.forward(relu3_out1);
        let add42_out1 = expand6_out1.clone().add(linear73_out1.clone());
        let transpose74_out1 = add42_out1.permute([1, 0, 2]);
        let transpose75_out1 = expand6_out1.clone().permute([1, 0, 2]);
        let shape162_out1: [i64; 3] = {
            let axes = &transpose74_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather144_out1 = shape162_out1[0] as i64;
        let gather145_out1 = shape162_out1[1] as i64;
        let gather146_out1 = shape162_out1[2] as i64;
        let constant1037_out1 = 8i64;
        let div60_out1 = gather146_out1 / constant1037_out1;
        let linear74_out1 = self.linear74.forward(transpose74_out1.clone());
        let linear75_out1 = self.linear75.forward(transpose74_out1);
        let linear76_out1 = self.linear76.forward(transpose75_out1);
        let constant1038_out1 = 8i64;
        let mul108_out1 = gather145_out1 * constant1038_out1;
        let unsqueeze180_out1 = [gather144_out1 as i64];
        let unsqueeze181_out1 = [mul108_out1 as i64];
        let unsqueeze182_out1 = [div60_out1 as i64];
        let concat97_out1: [i64; 3usize] = [
            &unsqueeze180_out1[..],
            &unsqueeze181_out1[..],
            &unsqueeze182_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape81_out1 = linear74_out1.reshape(concat97_out1);
        let transpose76_out1 = reshape81_out1.permute([1, 0, 2]);
        let shape165_out1: [i64; 3] = {
            let axes = &linear75_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather147_out1 = shape165_out1[0] as i64;
        let unsqueeze183_out1 = [gather147_out1 as i64];
        let unsqueeze184_out1 = [mul108_out1 as i64];
        let unsqueeze185_out1 = [div60_out1 as i64];
        let concat98_out1: [i64; 3usize] = [
            &unsqueeze183_out1[..],
            &unsqueeze184_out1[..],
            &unsqueeze185_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape82_out1 = linear75_out1.reshape(concat98_out1);
        let transpose77_out1 = reshape82_out1.permute([1, 0, 2]);
        let shape166_out1: [i64; 3] = {
            let axes = &linear76_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather148_out1 = shape166_out1[0] as i64;
        let unsqueeze186_out1 = [gather148_out1 as i64];
        let unsqueeze187_out1 = [mul108_out1 as i64];
        let unsqueeze188_out1 = [div60_out1 as i64];
        let concat99_out1: [i64; 3usize] = [
            &unsqueeze186_out1[..],
            &unsqueeze187_out1[..],
            &unsqueeze188_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape83_out1 = linear76_out1.reshape(concat99_out1);
        let transpose78_out1 = reshape83_out1.permute([1, 0, 2]);
        let shape167_out1: [i64; 3] = {
            let axes = &transpose77_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather149_out1 = shape167_out1[1] as i64;
        let unsqueeze189_out1 = [gather145_out1 as i64];
        let unsqueeze190_out1 = [gather144_out1 as i64];
        let unsqueeze191_out1 = [div60_out1 as i64];
        let constant1052_out1: [i64; 1] = [8i64];
        let concat100_out1: [i64; 4usize] = [
            &unsqueeze189_out1[..],
            &constant1052_out1[..],
            &unsqueeze190_out1[..],
            &unsqueeze191_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape84_out1 = transpose76_out1.reshape(concat100_out1);
        let unsqueeze192_out1 = [gather145_out1 as i64];
        let unsqueeze193_out1 = [gather149_out1 as i64];
        let unsqueeze194_out1 = [div60_out1 as i64];
        let constant1056_out1: [i64; 1] = [8i64];
        let concat101_out1: [i64; 4usize] = [
            &unsqueeze192_out1[..],
            &constant1056_out1[..],
            &unsqueeze193_out1[..],
            &unsqueeze194_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze195_out1 = [gather145_out1 as i64];
        let unsqueeze196_out1 = [gather149_out1 as i64];
        let unsqueeze197_out1 = [div60_out1 as i64];
        let constant1060_out1: [i64; 1] = [8i64];
        let concat102_out1: [i64; 4usize] = [
            &unsqueeze195_out1[..],
            &constant1060_out1[..],
            &unsqueeze196_out1[..],
            &unsqueeze197_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape85_out1 = transpose77_out1.reshape(concat101_out1);
        let reshape86_out1 = transpose78_out1.reshape(concat102_out1);
        let shape168_out1: [i64; 4] = {
            let axes = &reshape84_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice43_out1: [i64; 1] = shape168_out1[3..4].try_into().unwrap();
        let cast101_out1 = {
            let shape_array = slice43_out1 as [i64; 1usize];
            let float_array: [f64; 1usize] = shape_array.map(|x| x as f64);
            Tensor::<
                B,
                1,
            >::from_data(
                TensorData::from(float_array),
                (&self.device, burn::tensor::DType::F32),
            )
        };
        let sqrt34_out1 = cast101_out1.sqrt();
        let constant1065_out1 = self.constant1065.val();
        let div61_out1 = constant1065_out1.div(sqrt34_out1);
        let transpose79_out1 = reshape85_out1.permute([0, 1, 3, 2]);
        let sqrt35_out1 = div61_out1.sqrt();
        let mul109_out1 = reshape84_out1
            .mul((sqrt35_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul110_out1 = transpose79_out1
            .mul((sqrt35_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul99_out1 = mul109_out1.matmul(mul110_out1);
        let softmax12_out1 = burn::tensor::activation::softmax(matmul99_out1, 3);
        let matmul100_out1 = softmax12_out1.matmul(reshape86_out1);
        let transpose80_out1 = matmul100_out1.permute([2, 0, 1, 3]);
        let mul111_out1 = gather145_out1 * gather144_out1;
        let unsqueeze198_out1 = [mul111_out1 as i64];
        let unsqueeze199_out1 = [gather146_out1 as i64];
        let concat103_out1: [i64; 2usize] = [
            &unsqueeze198_out1[..],
            &unsqueeze199_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape87_out1 = transpose80_out1.reshape(concat103_out1);
        let linear77_out1 = self.linear77.forward(reshape87_out1);
        let shape169_out1: [i64; 2] = {
            let axes = &linear77_out1.clone().dims()[0..2];
            let mut output = [0i64; 2];
            for i in 0..2 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather150_out1 = shape169_out1[1] as i64;
        let unsqueeze200_out1 = [gather144_out1 as i64];
        let unsqueeze201_out1 = [gather145_out1 as i64];
        let unsqueeze202_out1 = [gather150_out1 as i64];
        let concat104_out1: [i64; 3usize] = [
            &unsqueeze200_out1[..],
            &unsqueeze201_out1[..],
            &unsqueeze202_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape88_out1 = linear77_out1.reshape(concat104_out1);
        let transpose81_out1 = reshape88_out1.permute([1, 0, 2]);
        let add43_out1 = expand6_out1.add(transpose81_out1);
        (add43_out1, linear73_out1, unsqueeze179_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule16<B: Backend> {
    layernormalization37: LayerNorm<B>,
    linear78: Linear<B>,
    linear79: Linear<B>,
    linear80: Linear<B>,
    constant1090: burn::module::Param<Tensor<B, 1>>,
    constant1095: burn::module::Param<Tensor<B, 1>>,
    constant1118: burn::module::Param<Tensor<B, 1>>,
    constant1119: burn::module::Param<Tensor<B, 1>>,
    linear81: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule16<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization37 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear78 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear79 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear80 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let constant1090: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1095: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant1118: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1119: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear81 = LinearConfig::new(256, 256).with_bias(true).init(device);
        Self {
            layernormalization37,
            linear78,
            linear79,
            linear80,
            constant1090,
            constant1095,
            constant1118,
            constant1119,
            linear81,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add43_out1: Tensor<B, 3>,
        linear73_out1: Tensor<B, 3>,
        shape154_out1: [i64; 3],
        transpose73_out1: Tensor<B, 3>,
        gather138_out1: i64,
        unsqueeze179_out1: Tensor<B, 4>,
        mul97_out1: i64,
        gather134_out1: i64,
        gather135_out1: i64,
    ) -> (Tensor<B, 3>, Tensor<B, 6>, Tensor<B, 6>, [i64; 4], [i64; 4]) {
        let layernormalization37_out1 = {
            let dtype = add43_out1.dtype();
            self.layernormalization37
                .forward(add43_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add44_out1 = layernormalization37_out1.clone().add(linear73_out1);
        let shape170_out1: [i64; 3] = {
            let axes = &add44_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather151_out1 = shape170_out1[1] as i64;
        let gather152_out1 = shape154_out1[1] as i64;
        let linear78_out1 = self.linear78.forward(transpose73_out1);
        let linear79_out1 = self.linear79.forward(add44_out1.clone());
        let unsqueeze203_out1 = [gather138_out1 as i64];
        let unsqueeze204_out1 = [gather151_out1 as i64];
        let constant1079_out1: [i64; 1] = [2i64];
        let constant1076_out1: [i64; 1] = [16i64];
        let constant1077_out1: [i64; 1] = [1i64];
        let constant1078_out1: [i64; 1] = [2i64];
        let concat105_out1: [i64; 6usize] = [
            &unsqueeze203_out1[..],
            &unsqueeze204_out1[..],
            &constant1076_out1[..],
            &constant1077_out1[..],
            &constant1078_out1[..],
            &constant1079_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape89_out1 = linear79_out1.reshape(concat105_out1);
        let linear80_out1 = self.linear80.forward(add44_out1);
        let unsqueeze205_out1 = [gather138_out1 as i64];
        let unsqueeze206_out1 = [gather151_out1 as i64];
        let constant1083_out1: [i64; 1] = [2i64];
        let constant1082_out1: [i64; 1] = [16i64];
        let concat106_out1: [i64; 4usize] = [
            &unsqueeze205_out1[..],
            &unsqueeze206_out1[..],
            &constant1082_out1[..],
            &constant1083_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape90_out1 = linear80_out1.reshape(concat106_out1);
        let unsqueeze207_out1: Tensor<B, 5> = unsqueeze179_out1
            .unsqueeze_dims::<5>(&[2]);
        let unsqueeze208_out1: Tensor<B, 6> = unsqueeze207_out1
            .unsqueeze_dims::<6>(&[4]);
        let slice44_out1 = unsqueeze208_out1.clone().slice(s![.., .., .., .., .., 0..2]);
        let constant1090_out1 = self.constant1090.val();
        let div62_out1 = reshape89_out1
            .div(
                (constant1090_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let slice45_out1 = unsqueeze208_out1.slice(s![.., .., .., .., .., 2..]);
        let mul112_out1 = div62_out1.mul(slice45_out1.clone());
        let constant1095_out1 = self.constant1095.val();
        let mul113_out1 = mul112_out1
            .mul(
                (constant1095_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add45_out1 = slice44_out1.clone().add(mul113_out1);
        let softmax13_out1 = burn::tensor::activation::softmax(reshape90_out1, 3);
        let transpose82_out1 = linear78_out1.permute([0, 2, 1]);
        let unsqueeze209_out1 = [gather138_out1 as i64];
        let unsqueeze210_out1 = [gather152_out1 as i64];
        let constant1097_out1: [i64; 1] = [16i64];
        let constant1098_out1: [i64; 1] = [16i64];
        let concat107_out1: [i64; 4usize] = [
            &unsqueeze209_out1[..],
            &constant1097_out1[..],
            &constant1098_out1[..],
            &unsqueeze210_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze211_out1 = [gather138_out1 as i64];
        let unsqueeze212_out1 = [gather152_out1 as i64];
        let constant1101_out1: [i64; 1] = [16i64];
        let constant1102_out1: [i64; 1] = [16i64];
        let concat108_out1: [i64; 4usize] = [
            &unsqueeze211_out1[..],
            &constant1101_out1[..],
            &constant1102_out1[..],
            &unsqueeze212_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze213_out1 = [gather138_out1 as i64];
        let unsqueeze214_out1 = [gather152_out1 as i64];
        let constant1105_out1: [i64; 1] = [16i64];
        let constant1106_out1: [i64; 1] = [16i64];
        let concat109_out1: [i64; 4usize] = [
            &unsqueeze213_out1[..],
            &constant1105_out1[..],
            &constant1106_out1[..],
            &unsqueeze214_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape91_out1 = transpose82_out1.reshape(concat107_out1);
        let shape172_out1: [i64; 4] = {
            let axes = &reshape91_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather153_out1 = shape172_out1[0] as i64;
        let gather154_out1 = shape172_out1[2] as i64;
        let shape174_out1: [i64; 6] = {
            let axes = &add45_out1.clone().dims()[0..6];
            let mut output = [0i64; 6];
            for i in 0..6 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather155_out1 = shape174_out1[1] as i64;
        let gather156_out1 = shape174_out1[2] as i64;
        let gather157_out1 = shape174_out1[3] as i64;
        let gather158_out1 = shape174_out1[4] as i64;
        let unsqueeze215_out1 = [mul97_out1 as i64];
        let slice46_out1 = reshape91_out1.slice(s![.., .., .., 0..unsqueeze215_out1[0]]);
        let constant1118_out1 = self.constant1118.val();
        let mul114_out1 = add45_out1
            .mul(
                (constant1118_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let constant1119_out1 = self.constant1119.val();
        let sub7_out1 = mul114_out1
            .sub(
                (constant1119_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul115_out1 = gather153_out1 * gather156_out1;
        let unsqueeze216_out1 = [mul115_out1 as i64];
        let unsqueeze217_out1 = [gather154_out1 as i64];
        let unsqueeze218_out1 = [gather134_out1 as i64];
        let unsqueeze219_out1 = [gather135_out1 as i64];
        let concat110_out1: [i64; 4usize] = [
            &unsqueeze216_out1[..],
            &unsqueeze217_out1[..],
            &unsqueeze218_out1[..],
            &unsqueeze219_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape92_out1 = slice46_out1.reshape(concat110_out1);
        let gather159_out1 = {
            let sliced = sub7_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose83_out1 = gather159_out1.permute([0, 2, 1, 3, 4]);
        let shape178_out1: [i64; 5] = {
            let axes = &transpose83_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice47_out1: [i64; 0] = shape178_out1[0..0].try_into().unwrap();
        let slice48_out1: [i64; 3] = shape178_out1[2..5].try_into().unwrap();
        let constant1130_out1: [i64; 1] = [-1i64];
        let concat111_out1: [i64; 4usize] = [
            &slice47_out1[..],
            &constant1130_out1[..],
            &slice48_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape93_out1 = transpose83_out1.reshape(concat111_out1);
        let gridsample1_out1 = reshape92_out1
            .grid_sample_2d(
                reshape93_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose84_out1 = softmax13_out1.permute([0, 2, 1, 3]);
        let mul116_out1 = gather157_out1 * gather158_out1;
        let unsqueeze220_out1 = [mul115_out1 as i64];
        let unsqueeze221_out1 = [gather155_out1 as i64];
        let unsqueeze222_out1 = [mul116_out1 as i64];
        let constant1132_out1: [i64; 1] = [1i64];
        let concat112_out1: [i64; 4usize] = [
            &unsqueeze220_out1[..],
            &constant1132_out1[..],
            &unsqueeze221_out1[..],
            &unsqueeze222_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape94_out1 = transpose84_out1.reshape(concat112_out1);
        let unsqueeze223_out1: Tensor<B, 5> = gridsample1_out1
            .unsqueeze_dims::<5>(&[-2]);
        let shape179_out1: [i64; 5] = {
            let axes = &unsqueeze223_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice49_out1: [i64; 3] = shape179_out1[0..3].try_into().unwrap();
        let constant1139_out1: [i64; 1] = [-1i64];
        let concat114_out1: [i64; 4usize] = [&slice49_out1[..], &constant1139_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape95_out1 = unsqueeze223_out1.reshape(concat114_out1);
        let mul117_out1 = reshape95_out1.mul(reshape94_out1);
        let reducesum2_out1 = {
            mul117_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let mul118_out1 = gather156_out1 * gather154_out1;
        let unsqueeze224_out1 = [gather153_out1 as i64];
        let unsqueeze225_out1 = [mul118_out1 as i64];
        let unsqueeze226_out1 = [gather155_out1 as i64];
        let concat115_out1: [i64; 3usize] = [
            &unsqueeze224_out1[..],
            &unsqueeze225_out1[..],
            &unsqueeze226_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape96_out1 = reducesum2_out1.reshape(concat115_out1);
        let transpose85_out1 = reshape96_out1.permute([0, 2, 1]);
        let linear81_out1 = self.linear81.forward(transpose85_out1);
        let add47_out1 = layernormalization37_out1.add(linear81_out1);
        (add47_out1, slice45_out1, slice44_out1, concat108_out1, concat109_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule17<B: Backend> {
    layernormalization38: LayerNorm<B>,
    linear82: Linear<B>,
    linear83: Linear<B>,
    layernormalization39: LayerNorm<B>,
    linear84: Linear<B>,
    linear85: Linear<B>,
    linear86: Linear<B>,
    constant1175: burn::module::Param<Tensor<B, 1>>,
    linear87: Linear<B>,
    layernormalization40: LayerNorm<B>,
    linear88: Linear<B>,
    linear89: Linear<B>,
    linear90: Linear<B>,
    constant1193: burn::module::Param<Tensor<B, 1>>,
    constant1194: burn::module::Param<Tensor<B, 1>>,
    constant1205: burn::module::Param<Tensor<B, 1>>,
    constant1206: burn::module::Param<Tensor<B, 1>>,
    linear91: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule17<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization38 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear82 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear83 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization39 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear84 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear85 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear86 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let constant1175: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([1], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1].into(),
        );
        let linear87 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        let layernormalization40 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear88 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear89 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear90 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let constant1193: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1194: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant1205: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1206: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear91 = LinearConfig::new(256, 256).with_bias(true).init(device);
        Self {
            layernormalization38,
            linear82,
            linear83,
            layernormalization39,
            linear84,
            linear85,
            linear86,
            constant1175,
            linear87,
            layernormalization40,
            linear88,
            linear89,
            linear90,
            constant1193,
            constant1194,
            constant1205,
            constant1206,
            linear91,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add47_out1: Tensor<B, 3>,
        linear73_out1: Tensor<B, 3>,
        transpose73_out1: Tensor<B, 3>,
        gather138_out1: i64,
        slice45_out1: Tensor<B, 6>,
        slice44_out1: Tensor<B, 6>,
        concat108_out1: [i64; 4],
        mul97_out1: i64,
        gather134_out1: i64,
        gather135_out1: i64,
    ) -> Tensor<B, 3> {
        let layernormalization38_out1 = {
            let dtype = add47_out1.dtype();
            self.layernormalization38
                .forward(add47_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear82_out1 = self.linear82.forward(layernormalization38_out1.clone());
        let relu4_out1 = burn::tensor::activation::relu(linear82_out1);
        let linear83_out1 = self.linear83.forward(relu4_out1);
        let add48_out1 = layernormalization38_out1.add(linear83_out1);
        let layernormalization39_out1 = {
            let dtype = add48_out1.dtype();
            self.layernormalization39
                .forward(add48_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add49_out1 = layernormalization39_out1.clone().add(linear73_out1.clone());
        let transpose86_out1 = add49_out1.permute([1, 0, 2]);
        let transpose87_out1 = layernormalization39_out1.clone().permute([1, 0, 2]);
        let shape180_out1: [i64; 3] = {
            let axes = &transpose86_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather160_out1 = shape180_out1[0] as i64;
        let gather161_out1 = shape180_out1[1] as i64;
        let gather162_out1 = shape180_out1[2] as i64;
        let constant1147_out1 = 8i64;
        let div63_out1 = gather162_out1 / constant1147_out1;
        let linear84_out1 = self.linear84.forward(transpose86_out1.clone());
        let linear85_out1 = self.linear85.forward(transpose86_out1);
        let linear86_out1 = self.linear86.forward(transpose87_out1);
        let constant1148_out1 = 8i64;
        let mul119_out1 = gather161_out1 * constant1148_out1;
        let unsqueeze227_out1 = [gather160_out1 as i64];
        let unsqueeze228_out1 = [mul119_out1 as i64];
        let unsqueeze229_out1 = [div63_out1 as i64];
        let concat116_out1: [i64; 3usize] = [
            &unsqueeze227_out1[..],
            &unsqueeze228_out1[..],
            &unsqueeze229_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape97_out1 = linear84_out1.reshape(concat116_out1);
        let transpose88_out1 = reshape97_out1.permute([1, 0, 2]);
        let shape183_out1: [i64; 3] = {
            let axes = &linear85_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather163_out1 = shape183_out1[0] as i64;
        let unsqueeze230_out1 = [gather163_out1 as i64];
        let unsqueeze231_out1 = [mul119_out1 as i64];
        let unsqueeze232_out1 = [div63_out1 as i64];
        let concat117_out1: [i64; 3usize] = [
            &unsqueeze230_out1[..],
            &unsqueeze231_out1[..],
            &unsqueeze232_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape98_out1 = linear85_out1.reshape(concat117_out1);
        let transpose89_out1 = reshape98_out1.permute([1, 0, 2]);
        let shape184_out1: [i64; 3] = {
            let axes = &linear86_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather164_out1 = shape184_out1[0] as i64;
        let unsqueeze233_out1 = [gather164_out1 as i64];
        let unsqueeze234_out1 = [mul119_out1 as i64];
        let unsqueeze235_out1 = [div63_out1 as i64];
        let concat118_out1: [i64; 3usize] = [
            &unsqueeze233_out1[..],
            &unsqueeze234_out1[..],
            &unsqueeze235_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape99_out1 = linear86_out1.reshape(concat118_out1);
        let transpose90_out1 = reshape99_out1.permute([1, 0, 2]);
        let shape185_out1: [i64; 3] = {
            let axes = &transpose89_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather165_out1 = shape185_out1[1] as i64;
        let unsqueeze236_out1 = [gather161_out1 as i64];
        let unsqueeze237_out1 = [gather160_out1 as i64];
        let unsqueeze238_out1 = [div63_out1 as i64];
        let constant1162_out1: [i64; 1] = [8i64];
        let concat119_out1: [i64; 4usize] = [
            &unsqueeze236_out1[..],
            &constant1162_out1[..],
            &unsqueeze237_out1[..],
            &unsqueeze238_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape100_out1 = transpose88_out1.reshape(concat119_out1);
        let unsqueeze239_out1 = [gather161_out1 as i64];
        let unsqueeze240_out1 = [gather165_out1 as i64];
        let unsqueeze241_out1 = [div63_out1 as i64];
        let constant1166_out1: [i64; 1] = [8i64];
        let concat120_out1: [i64; 4usize] = [
            &unsqueeze239_out1[..],
            &constant1166_out1[..],
            &unsqueeze240_out1[..],
            &unsqueeze241_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze242_out1 = [gather161_out1 as i64];
        let unsqueeze243_out1 = [gather165_out1 as i64];
        let unsqueeze244_out1 = [div63_out1 as i64];
        let constant1170_out1: [i64; 1] = [8i64];
        let concat121_out1: [i64; 4usize] = [
            &unsqueeze242_out1[..],
            &constant1170_out1[..],
            &unsqueeze243_out1[..],
            &unsqueeze244_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape101_out1 = transpose89_out1.reshape(concat120_out1);
        let reshape102_out1 = transpose90_out1.reshape(concat121_out1);
        let shape186_out1: [i64; 4] = {
            let axes = &reshape100_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice50_out1: [i64; 1] = shape186_out1[3..4].try_into().unwrap();
        let cast105_out1 = {
            let shape_array = slice50_out1 as [i64; 1usize];
            let float_array: [f64; 1usize] = shape_array.map(|x| x as f64);
            Tensor::<
                B,
                1,
            >::from_data(
                TensorData::from(float_array),
                (&self.device, burn::tensor::DType::F32),
            )
        };
        let sqrt37_out1 = cast105_out1.sqrt();
        let constant1175_out1 = self.constant1175.val();
        let div64_out1 = constant1175_out1.div(sqrt37_out1);
        let transpose91_out1 = reshape101_out1.permute([0, 1, 3, 2]);
        let sqrt38_out1 = div64_out1.sqrt();
        let mul120_out1 = reshape100_out1
            .mul((sqrt38_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul121_out1 = transpose91_out1
            .mul((sqrt38_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul110_out1 = mul120_out1.matmul(mul121_out1);
        let softmax14_out1 = burn::tensor::activation::softmax(matmul110_out1, 3);
        let matmul111_out1 = softmax14_out1.matmul(reshape102_out1);
        let transpose92_out1 = matmul111_out1.permute([2, 0, 1, 3]);
        let mul122_out1 = gather161_out1 * gather160_out1;
        let unsqueeze245_out1 = [mul122_out1 as i64];
        let unsqueeze246_out1 = [gather162_out1 as i64];
        let concat122_out1: [i64; 2usize] = [
            &unsqueeze245_out1[..],
            &unsqueeze246_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape103_out1 = transpose92_out1.reshape(concat122_out1);
        let linear87_out1 = self.linear87.forward(reshape103_out1);
        let shape187_out1: [i64; 2] = {
            let axes = &linear87_out1.clone().dims()[0..2];
            let mut output = [0i64; 2];
            for i in 0..2 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather166_out1 = shape187_out1[1] as i64;
        let unsqueeze247_out1 = [gather160_out1 as i64];
        let unsqueeze248_out1 = [gather161_out1 as i64];
        let unsqueeze249_out1 = [gather166_out1 as i64];
        let concat123_out1: [i64; 3usize] = [
            &unsqueeze247_out1[..],
            &unsqueeze248_out1[..],
            &unsqueeze249_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape104_out1 = linear87_out1.reshape(concat123_out1);
        let transpose93_out1 = reshape104_out1.permute([1, 0, 2]);
        let add50_out1 = layernormalization39_out1.add(transpose93_out1);
        let layernormalization40_out1 = {
            let dtype = add50_out1.dtype();
            self.layernormalization40
                .forward(add50_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add51_out1 = layernormalization40_out1.clone().add(linear73_out1);
        let shape188_out1: [i64; 3] = {
            let axes = &add51_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather167_out1 = shape188_out1[1] as i64;
        let linear88_out1 = self.linear88.forward(transpose73_out1);
        let linear89_out1 = self.linear89.forward(add51_out1.clone());
        let unsqueeze250_out1 = [gather138_out1 as i64];
        let unsqueeze251_out1 = [gather167_out1 as i64];
        let constant1188_out1: [i64; 1] = [2i64];
        let constant1185_out1: [i64; 1] = [16i64];
        let constant1186_out1: [i64; 1] = [1i64];
        let constant1187_out1: [i64; 1] = [2i64];
        let concat124_out1: [i64; 6usize] = [
            &unsqueeze250_out1[..],
            &unsqueeze251_out1[..],
            &constant1185_out1[..],
            &constant1186_out1[..],
            &constant1187_out1[..],
            &constant1188_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape105_out1 = linear89_out1.reshape(concat124_out1);
        let linear90_out1 = self.linear90.forward(add51_out1);
        let unsqueeze252_out1 = [gather138_out1 as i64];
        let unsqueeze253_out1 = [gather167_out1 as i64];
        let constant1192_out1: [i64; 1] = [2i64];
        let constant1191_out1: [i64; 1] = [16i64];
        let concat125_out1: [i64; 4usize] = [
            &unsqueeze252_out1[..],
            &unsqueeze253_out1[..],
            &constant1191_out1[..],
            &constant1192_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape106_out1 = linear90_out1.reshape(concat125_out1);
        let constant1193_out1 = self.constant1193.val();
        let div65_out1 = reshape105_out1
            .div(
                (constant1193_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul123_out1 = div65_out1.mul(slice45_out1);
        let constant1194_out1 = self.constant1194.val();
        let mul124_out1 = mul123_out1
            .mul(
                (constant1194_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add52_out1 = slice44_out1.add(mul124_out1);
        let softmax15_out1 = burn::tensor::activation::softmax(reshape106_out1, 3);
        let transpose94_out1 = linear88_out1.permute([0, 2, 1]);
        let reshape107_out1 = transpose94_out1.reshape(concat108_out1);
        let shape189_out1: [i64; 4] = {
            let axes = &reshape107_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather168_out1 = shape189_out1[0] as i64;
        let gather169_out1 = shape189_out1[2] as i64;
        let shape191_out1: [i64; 6] = {
            let axes = &add52_out1.clone().dims()[0..6];
            let mut output = [0i64; 6];
            for i in 0..6 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather170_out1 = shape191_out1[1] as i64;
        let gather171_out1 = shape191_out1[2] as i64;
        let gather172_out1 = shape191_out1[3] as i64;
        let gather173_out1 = shape191_out1[4] as i64;
        let unsqueeze254_out1 = [mul97_out1 as i64];
        let slice51_out1 = reshape107_out1
            .slice(s![.., .., .., 0..unsqueeze254_out1[0]]);
        let constant1205_out1 = self.constant1205.val();
        let mul125_out1 = add52_out1
            .mul(
                (constant1205_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let constant1206_out1 = self.constant1206.val();
        let sub8_out1 = mul125_out1
            .sub(
                (constant1206_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul126_out1 = gather168_out1 * gather171_out1;
        let unsqueeze255_out1 = [mul126_out1 as i64];
        let unsqueeze256_out1 = [gather169_out1 as i64];
        let unsqueeze257_out1 = [gather134_out1 as i64];
        let unsqueeze258_out1 = [gather135_out1 as i64];
        let concat126_out1: [i64; 4usize] = [
            &unsqueeze255_out1[..],
            &unsqueeze256_out1[..],
            &unsqueeze257_out1[..],
            &unsqueeze258_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape108_out1 = slice51_out1.reshape(concat126_out1);
        let gather174_out1 = {
            let sliced = sub8_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose95_out1 = gather174_out1.permute([0, 2, 1, 3, 4]);
        let shape195_out1: [i64; 5] = {
            let axes = &transpose95_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice52_out1: [i64; 0] = shape195_out1[0..0].try_into().unwrap();
        let slice53_out1: [i64; 3] = shape195_out1[2..5].try_into().unwrap();
        let constant1217_out1: [i64; 1] = [-1i64];
        let concat127_out1: [i64; 4usize] = [
            &slice52_out1[..],
            &constant1217_out1[..],
            &slice53_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape109_out1 = transpose95_out1.reshape(concat127_out1);
        let gridsample2_out1 = reshape108_out1
            .grid_sample_2d(
                reshape109_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose96_out1 = softmax15_out1.permute([0, 2, 1, 3]);
        let mul127_out1 = gather172_out1 * gather173_out1;
        let unsqueeze259_out1 = [mul126_out1 as i64];
        let unsqueeze260_out1 = [gather170_out1 as i64];
        let unsqueeze261_out1 = [mul127_out1 as i64];
        let constant1219_out1: [i64; 1] = [1i64];
        let concat128_out1: [i64; 4usize] = [
            &unsqueeze259_out1[..],
            &constant1219_out1[..],
            &unsqueeze260_out1[..],
            &unsqueeze261_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape110_out1 = transpose96_out1.reshape(concat128_out1);
        let unsqueeze262_out1: Tensor<B, 5> = gridsample2_out1
            .unsqueeze_dims::<5>(&[-2]);
        let shape196_out1: [i64; 5] = {
            let axes = &unsqueeze262_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice54_out1: [i64; 3] = shape196_out1[0..3].try_into().unwrap();
        let constant1226_out1: [i64; 1] = [-1i64];
        let concat130_out1: [i64; 4usize] = [&slice54_out1[..], &constant1226_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape111_out1 = unsqueeze262_out1.reshape(concat130_out1);
        let mul128_out1 = reshape111_out1.mul(reshape110_out1);
        let reducesum3_out1 = {
            mul128_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let mul129_out1 = gather171_out1 * gather169_out1;
        let unsqueeze263_out1 = [gather168_out1 as i64];
        let unsqueeze264_out1 = [mul129_out1 as i64];
        let unsqueeze265_out1 = [gather170_out1 as i64];
        let concat131_out1: [i64; 3usize] = [
            &unsqueeze263_out1[..],
            &unsqueeze264_out1[..],
            &unsqueeze265_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape112_out1 = reducesum3_out1.reshape(concat131_out1);
        let transpose97_out1 = reshape112_out1.permute([0, 2, 1]);
        let linear91_out1 = self.linear91.forward(transpose97_out1);
        let add54_out1 = layernormalization40_out1.add(linear91_out1);
        add54_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule18<B: Backend> {
    layernormalization41: LayerNorm<B>,
    linear92: Linear<B>,
    linear93: Linear<B>,
    layernormalization42: LayerNorm<B>,
    linear94: Linear<B>,
    linear95: Linear<B>,
    linear96: Linear<B>,
    constant1261: burn::module::Param<Tensor<B, 1>>,
    linear97: Linear<B>,
    layernormalization43: LayerNorm<B>,
    linear98: Linear<B>,
    linear99: Linear<B>,
    linear100: Linear<B>,
    constant1279: burn::module::Param<Tensor<B, 1>>,
    constant1280: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule18<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization41 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear92 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear93 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization42 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear94 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear95 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear96 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let constant1261: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([1], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1].into(),
        );
        let linear97 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        let layernormalization43 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear98 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear99 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear100 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let constant1279: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1280: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        Self {
            layernormalization41,
            linear92,
            linear93,
            layernormalization42,
            linear94,
            linear95,
            linear96,
            constant1261,
            linear97,
            layernormalization43,
            linear98,
            linear99,
            linear100,
            constant1279,
            constant1280,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add54_out1: Tensor<B, 3>,
        linear73_out1: Tensor<B, 3>,
        transpose73_out1: Tensor<B, 3>,
        gather138_out1: i64,
        slice45_out1: Tensor<B, 6>,
        slice44_out1: Tensor<B, 6>,
        concat109_out1: [i64; 4],
    ) -> (Tensor<B, 4>, Tensor<B, 6>, Tensor<B, 4>, Tensor<B, 3>) {
        let layernormalization41_out1 = {
            let dtype = add54_out1.dtype();
            self.layernormalization41
                .forward(add54_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear92_out1 = self.linear92.forward(layernormalization41_out1.clone());
        let relu5_out1 = burn::tensor::activation::relu(linear92_out1);
        let linear93_out1 = self.linear93.forward(relu5_out1);
        let add55_out1 = layernormalization41_out1.add(linear93_out1);
        let layernormalization42_out1 = {
            let dtype = add55_out1.dtype();
            self.layernormalization42
                .forward(add55_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add56_out1 = layernormalization42_out1.clone().add(linear73_out1.clone());
        let transpose98_out1 = add56_out1.permute([1, 0, 2]);
        let transpose99_out1 = layernormalization42_out1.clone().permute([1, 0, 2]);
        let shape197_out1: [i64; 3] = {
            let axes = &transpose98_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather175_out1 = shape197_out1[0] as i64;
        let gather176_out1 = shape197_out1[1] as i64;
        let gather177_out1 = shape197_out1[2] as i64;
        let constant1233_out1 = 8i64;
        let div66_out1 = gather177_out1 / constant1233_out1;
        let linear94_out1 = self.linear94.forward(transpose98_out1.clone());
        let linear95_out1 = self.linear95.forward(transpose98_out1);
        let linear96_out1 = self.linear96.forward(transpose99_out1);
        let constant1234_out1 = 8i64;
        let mul130_out1 = gather176_out1 * constant1234_out1;
        let unsqueeze266_out1 = [gather175_out1 as i64];
        let unsqueeze267_out1 = [mul130_out1 as i64];
        let unsqueeze268_out1 = [div66_out1 as i64];
        let concat132_out1: [i64; 3usize] = [
            &unsqueeze266_out1[..],
            &unsqueeze267_out1[..],
            &unsqueeze268_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape113_out1 = linear94_out1.reshape(concat132_out1);
        let transpose100_out1 = reshape113_out1.permute([1, 0, 2]);
        let shape200_out1: [i64; 3] = {
            let axes = &linear95_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather178_out1 = shape200_out1[0] as i64;
        let unsqueeze269_out1 = [gather178_out1 as i64];
        let unsqueeze270_out1 = [mul130_out1 as i64];
        let unsqueeze271_out1 = [div66_out1 as i64];
        let concat133_out1: [i64; 3usize] = [
            &unsqueeze269_out1[..],
            &unsqueeze270_out1[..],
            &unsqueeze271_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape114_out1 = linear95_out1.reshape(concat133_out1);
        let transpose101_out1 = reshape114_out1.permute([1, 0, 2]);
        let shape201_out1: [i64; 3] = {
            let axes = &linear96_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather179_out1 = shape201_out1[0] as i64;
        let unsqueeze272_out1 = [gather179_out1 as i64];
        let unsqueeze273_out1 = [mul130_out1 as i64];
        let unsqueeze274_out1 = [div66_out1 as i64];
        let concat134_out1: [i64; 3usize] = [
            &unsqueeze272_out1[..],
            &unsqueeze273_out1[..],
            &unsqueeze274_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape115_out1 = linear96_out1.reshape(concat134_out1);
        let transpose102_out1 = reshape115_out1.permute([1, 0, 2]);
        let shape202_out1: [i64; 3] = {
            let axes = &transpose101_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather180_out1 = shape202_out1[1] as i64;
        let unsqueeze275_out1 = [gather176_out1 as i64];
        let unsqueeze276_out1 = [gather175_out1 as i64];
        let unsqueeze277_out1 = [div66_out1 as i64];
        let constant1248_out1: [i64; 1] = [8i64];
        let concat135_out1: [i64; 4usize] = [
            &unsqueeze275_out1[..],
            &constant1248_out1[..],
            &unsqueeze276_out1[..],
            &unsqueeze277_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape116_out1 = transpose100_out1.reshape(concat135_out1);
        let unsqueeze278_out1 = [gather176_out1 as i64];
        let unsqueeze279_out1 = [gather180_out1 as i64];
        let unsqueeze280_out1 = [div66_out1 as i64];
        let constant1252_out1: [i64; 1] = [8i64];
        let concat136_out1: [i64; 4usize] = [
            &unsqueeze278_out1[..],
            &constant1252_out1[..],
            &unsqueeze279_out1[..],
            &unsqueeze280_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let unsqueeze281_out1 = [gather176_out1 as i64];
        let unsqueeze282_out1 = [gather180_out1 as i64];
        let unsqueeze283_out1 = [div66_out1 as i64];
        let constant1256_out1: [i64; 1] = [8i64];
        let concat137_out1: [i64; 4usize] = [
            &unsqueeze281_out1[..],
            &constant1256_out1[..],
            &unsqueeze282_out1[..],
            &unsqueeze283_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape117_out1 = transpose101_out1.reshape(concat136_out1);
        let reshape118_out1 = transpose102_out1.reshape(concat137_out1);
        let shape203_out1: [i64; 4] = {
            let axes = &reshape116_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice55_out1: [i64; 1] = shape203_out1[3..4].try_into().unwrap();
        let cast109_out1 = {
            let shape_array = slice55_out1 as [i64; 1usize];
            let float_array: [f64; 1usize] = shape_array.map(|x| x as f64);
            Tensor::<
                B,
                1,
            >::from_data(
                TensorData::from(float_array),
                (&self.device, burn::tensor::DType::F32),
            )
        };
        let sqrt40_out1 = cast109_out1.sqrt();
        let constant1261_out1 = self.constant1261.val();
        let div67_out1 = constant1261_out1.div(sqrt40_out1);
        let transpose103_out1 = reshape117_out1.permute([0, 1, 3, 2]);
        let sqrt41_out1 = div67_out1.sqrt();
        let mul131_out1 = reshape116_out1
            .mul((sqrt41_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul132_out1 = transpose103_out1
            .mul((sqrt41_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let matmul121_out1 = mul131_out1.matmul(mul132_out1);
        let softmax16_out1 = burn::tensor::activation::softmax(matmul121_out1, 3);
        let matmul122_out1 = softmax16_out1.matmul(reshape118_out1);
        let transpose104_out1 = matmul122_out1.permute([2, 0, 1, 3]);
        let mul133_out1 = gather176_out1 * gather175_out1;
        let unsqueeze284_out1 = [mul133_out1 as i64];
        let unsqueeze285_out1 = [gather177_out1 as i64];
        let concat138_out1: [i64; 2usize] = [
            &unsqueeze284_out1[..],
            &unsqueeze285_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape119_out1 = transpose104_out1.reshape(concat138_out1);
        let linear97_out1 = self.linear97.forward(reshape119_out1);
        let shape204_out1: [i64; 2] = {
            let axes = &linear97_out1.clone().dims()[0..2];
            let mut output = [0i64; 2];
            for i in 0..2 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather181_out1 = shape204_out1[1] as i64;
        let unsqueeze286_out1 = [gather175_out1 as i64];
        let unsqueeze287_out1 = [gather176_out1 as i64];
        let unsqueeze288_out1 = [gather181_out1 as i64];
        let concat139_out1: [i64; 3usize] = [
            &unsqueeze286_out1[..],
            &unsqueeze287_out1[..],
            &unsqueeze288_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape120_out1 = linear97_out1.reshape(concat139_out1);
        let transpose105_out1 = reshape120_out1.permute([1, 0, 2]);
        let add57_out1 = layernormalization42_out1.add(transpose105_out1);
        let layernormalization43_out1 = {
            let dtype = add57_out1.dtype();
            self.layernormalization43
                .forward(add57_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add58_out1 = layernormalization43_out1.clone().add(linear73_out1);
        let shape205_out1: [i64; 3] = {
            let axes = &add58_out1.clone().dims()[0..3];
            let mut output = [0i64; 3];
            for i in 0..3 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather182_out1 = shape205_out1[1] as i64;
        let linear98_out1 = self.linear98.forward(transpose73_out1);
        let linear99_out1 = self.linear99.forward(add58_out1.clone());
        let unsqueeze289_out1 = [gather138_out1 as i64];
        let unsqueeze290_out1 = [gather182_out1 as i64];
        let constant1274_out1: [i64; 1] = [2i64];
        let constant1271_out1: [i64; 1] = [16i64];
        let constant1272_out1: [i64; 1] = [1i64];
        let constant1273_out1: [i64; 1] = [2i64];
        let concat140_out1: [i64; 6usize] = [
            &unsqueeze289_out1[..],
            &unsqueeze290_out1[..],
            &constant1271_out1[..],
            &constant1272_out1[..],
            &constant1273_out1[..],
            &constant1274_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape121_out1 = linear99_out1.reshape(concat140_out1);
        let linear100_out1 = self.linear100.forward(add58_out1);
        let unsqueeze291_out1 = [gather138_out1 as i64];
        let unsqueeze292_out1 = [gather182_out1 as i64];
        let constant1278_out1: [i64; 1] = [2i64];
        let constant1277_out1: [i64; 1] = [16i64];
        let concat141_out1: [i64; 4usize] = [
            &unsqueeze291_out1[..],
            &unsqueeze292_out1[..],
            &constant1277_out1[..],
            &constant1278_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape122_out1 = linear100_out1.reshape(concat141_out1);
        let constant1279_out1 = self.constant1279.val();
        let div68_out1 = reshape121_out1
            .div(
                (constant1279_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul134_out1 = div68_out1.mul(slice45_out1);
        let constant1280_out1 = self.constant1280.val();
        let mul135_out1 = mul134_out1
            .mul(
                (constant1280_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add59_out1 = slice44_out1.add(mul135_out1);
        let softmax17_out1 = burn::tensor::activation::softmax(reshape122_out1, 3);
        let transpose106_out1 = linear98_out1.permute([0, 2, 1]);
        let reshape123_out1 = transpose106_out1.reshape(concat109_out1);
        (reshape123_out1, add59_out1, softmax17_out1, layernormalization43_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule19<B: Backend> {
    constant1291: burn::module::Param<Tensor<B, 1>>,
    constant1292: burn::module::Param<Tensor<B, 1>>,
    linear101: Linear<B>,
    layernormalization44: LayerNorm<B>,
    linear102: Linear<B>,
    linear103: Linear<B>,
    layernormalization45: LayerNorm<B>,
    layernormalization46: LayerNorm<B>,
    linear104: Linear<B>,
    linear105: Linear<B>,
    linear106: Linear<B>,
    linear107: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule19<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant1291: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F32),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant1292: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear101 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization44 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear102 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear103 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization45 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let layernormalization46 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear104 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear105 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear106 = LinearConfig::new(256, 4).with_bias(true).init(device);
        let linear107 = LinearConfig::new(256, 18).with_bias(true).init(device);
        Self {
            constant1291,
            constant1292,
            linear101,
            layernormalization44,
            linear102,
            linear103,
            layernormalization45,
            layernormalization46,
            linear104,
            linear105,
            linear106,
            linear107,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        reshape123_out1: Tensor<B, 4>,
        add59_out1: Tensor<B, 6>,
        mul97_out1: i64,
        gather134_out1: i64,
        gather135_out1: i64,
        softmax17_out1: Tensor<B, 4>,
        layernormalization43_out1: Tensor<B, 3>,
        concat87_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let shape206_out1: [i64; 4] = {
            let axes = &reshape123_out1.clone().dims()[0..4];
            let mut output = [0i64; 4];
            for i in 0..4 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather183_out1 = shape206_out1[0] as i64;
        let gather184_out1 = shape206_out1[2] as i64;
        let shape208_out1: [i64; 6] = {
            let axes = &add59_out1.clone().dims()[0..6];
            let mut output = [0i64; 6];
            for i in 0..6 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let gather185_out1 = shape208_out1[1] as i64;
        let gather186_out1 = shape208_out1[2] as i64;
        let gather187_out1 = shape208_out1[3] as i64;
        let gather188_out1 = shape208_out1[4] as i64;
        let unsqueeze293_out1 = [mul97_out1 as i64];
        let slice56_out1 = reshape123_out1
            .slice(s![.., .., .., 0..unsqueeze293_out1[0]]);
        let constant1291_out1 = self.constant1291.val();
        let mul136_out1 = add59_out1
            .mul(
                (constant1291_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let constant1292_out1 = self.constant1292.val();
        let sub9_out1 = mul136_out1
            .sub(
                (constant1292_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul137_out1 = gather183_out1 * gather186_out1;
        let unsqueeze294_out1 = [mul137_out1 as i64];
        let unsqueeze295_out1 = [gather184_out1 as i64];
        let unsqueeze296_out1 = [gather134_out1 as i64];
        let unsqueeze297_out1 = [gather135_out1 as i64];
        let concat142_out1: [i64; 4usize] = [
            &unsqueeze294_out1[..],
            &unsqueeze295_out1[..],
            &unsqueeze296_out1[..],
            &unsqueeze297_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape124_out1 = slice56_out1.reshape(concat142_out1);
        let gather189_out1 = {
            let sliced = sub9_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose107_out1 = gather189_out1.permute([0, 2, 1, 3, 4]);
        let shape212_out1: [i64; 5] = {
            let axes = &transpose107_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice57_out1: [i64; 0] = shape212_out1[0..0].try_into().unwrap();
        let slice58_out1: [i64; 3] = shape212_out1[2..5].try_into().unwrap();
        let constant1303_out1: [i64; 1] = [-1i64];
        let concat143_out1: [i64; 4usize] = [
            &slice57_out1[..],
            &constant1303_out1[..],
            &slice58_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape125_out1 = transpose107_out1.reshape(concat143_out1);
        let gridsample3_out1 = reshape124_out1
            .grid_sample_2d(
                reshape125_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose108_out1 = softmax17_out1.permute([0, 2, 1, 3]);
        let mul138_out1 = gather187_out1 * gather188_out1;
        let unsqueeze298_out1 = [mul137_out1 as i64];
        let unsqueeze299_out1 = [gather185_out1 as i64];
        let unsqueeze300_out1 = [mul138_out1 as i64];
        let constant1305_out1: [i64; 1] = [1i64];
        let concat144_out1: [i64; 4usize] = [
            &unsqueeze298_out1[..],
            &constant1305_out1[..],
            &unsqueeze299_out1[..],
            &unsqueeze300_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape126_out1 = transpose108_out1.reshape(concat144_out1);
        let unsqueeze301_out1: Tensor<B, 5> = gridsample3_out1
            .unsqueeze_dims::<5>(&[-2]);
        let shape213_out1: [i64; 5] = {
            let axes = &unsqueeze301_out1.clone().dims()[0..5];
            let mut output = [0i64; 5];
            for i in 0..5 {
                output[i] = axes[i] as i64;
            }
            output
        };
        let slice59_out1: [i64; 3] = shape213_out1[0..3].try_into().unwrap();
        let constant1312_out1: [i64; 1] = [-1i64];
        let concat146_out1: [i64; 4usize] = [&slice59_out1[..], &constant1312_out1[..]]
            .concat()
            .try_into()
            .unwrap();
        let reshape127_out1 = unsqueeze301_out1.reshape(concat146_out1);
        let mul139_out1 = reshape127_out1.mul(reshape126_out1);
        let reducesum4_out1 = {
            mul139_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let mul140_out1 = gather186_out1 * gather184_out1;
        let unsqueeze302_out1 = [gather183_out1 as i64];
        let unsqueeze303_out1 = [mul140_out1 as i64];
        let unsqueeze304_out1 = [gather185_out1 as i64];
        let concat147_out1: [i64; 3usize] = [
            &unsqueeze302_out1[..],
            &unsqueeze303_out1[..],
            &unsqueeze304_out1[..],
        ]
            .concat()
            .try_into()
            .unwrap();
        let reshape128_out1 = reducesum4_out1.reshape(concat147_out1);
        let transpose109_out1 = reshape128_out1.permute([0, 2, 1]);
        let linear101_out1 = self.linear101.forward(transpose109_out1);
        let add61_out1 = layernormalization43_out1.add(linear101_out1);
        let layernormalization44_out1 = {
            let dtype = add61_out1.dtype();
            self.layernormalization44
                .forward(add61_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear102_out1 = self.linear102.forward(layernormalization44_out1.clone());
        let relu6_out1 = burn::tensor::activation::relu(linear102_out1);
        let linear103_out1 = self.linear103.forward(relu6_out1);
        let add62_out1 = layernormalization44_out1.add(linear103_out1);
        let layernormalization45_out1 = {
            let dtype = add62_out1.dtype();
            self.layernormalization45
                .forward(add62_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let layernormalization46_out1 = {
            let dtype = layernormalization45_out1.dtype();
            self.layernormalization46
                .forward(layernormalization45_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear104_out1 = self.linear104.forward(layernormalization46_out1.clone());
        let relu7_out1 = burn::tensor::activation::relu(linear104_out1);
        let linear105_out1 = self.linear105.forward(relu7_out1);
        let relu8_out1 = burn::tensor::activation::relu(linear105_out1);
        let linear106_out1 = self.linear106.forward(relu8_out1);
        let slice60_out1 = linear106_out1.clone().slice(s![.., .., 0..2]);
        let slice61_out1 = concat87_out1.clone().slice(s![.., .., 2..]);
        let mul141_out1 = slice60_out1.mul(slice61_out1.clone());
        let slice62_out1 = concat87_out1.slice(s![.., .., 0..2]);
        let add63_out1 = mul141_out1.add(slice62_out1);
        let slice63_out1 = linear106_out1.slice(s![.., .., 2..]);
        let exp3_out1 = slice63_out1.exp();
        let mul142_out1 = exp3_out1.mul(slice61_out1);
        let concat148_out1 = burn::tensor::Tensor::cat(
            [add63_out1, mul142_out1].into(),
            2,
        );
        let linear107_out1 = self.linear107.forward(layernormalization46_out1);
        (concat148_out1, linear107_out1)
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
    submodule10: Submodule10<B>,
    submodule11: Submodule11<B>,
    submodule12: Submodule12<B>,
    submodule13: Submodule13<B>,
    submodule14: Submodule14<B>,
    submodule15: Submodule15<B>,
    submodule16: Submodule16<B>,
    submodule17: Submodule17<B>,
    submodule18: Submodule18<B>,
    submodule19: Submodule19<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}


extern crate std;

impl<B: Backend> Default for Model<B> {
    fn default() -> Self {
        Self::from_file(
            "rfdetr-base.bpk",
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
        let submodule10 = Submodule10::new(device);
        let submodule11 = Submodule11::new(device);
        let submodule12 = Submodule12::new(device);
        let submodule13 = Submodule13::new(device);
        let submodule14 = Submodule14::new(device);
        let submodule15 = Submodule15::new(device);
        let submodule16 = Submodule16::new(device);
        let submodule17 = Submodule17::new(device);
        let submodule18 = Submodule18::new(device);
        let submodule19 = Submodule19::new(device);
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
            submodule10,
            submodule11,
            submodule12,
            submodule13,
            submodule14,
            submodule15,
            submodule16,
            submodule17,
            submodule18,
            submodule19,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }

    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, input: Tensor<B, 4>) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let (concat7_out1, shape1_out1) = self.submodule1.forward(input);
        let add5_out1 = self.submodule2.forward(concat7_out1);
        let (reshape19_out1, add7_out1) = self.submodule3.forward(add5_out1);
        let add11_out1 = self.submodule4.forward(reshape19_out1, add7_out1.clone());
        let add16_out1 = self.submodule5.forward(add11_out1);
        let reshape33_out1 = self.submodule6.forward(add16_out1.clone());
        let add20_out1 = self.submodule7.forward(reshape33_out1, add16_out1.clone());
        let add25_out1 = self.submodule8.forward(add20_out1);
        let reshape47_out1 = self.submodule9.forward(add25_out1.clone());
        let add29_out1 = self.submodule10.forward(reshape47_out1, add25_out1.clone());
        let concat70_out1 = self
            .submodule11
            .forward(add29_out1, add7_out1, shape1_out1, add16_out1, add25_out1);
        let (shape150_out1, gather134_out1, gather135_out1, transpose73_out1) = self
            .submodule12
            .forward(concat70_out1);
        let (gatherelements1_out1, mul97_out1) = self
            .submodule13
            .forward(
                shape150_out1,
                gather134_out1.clone(),
                gather135_out1.clone(),
                transpose73_out1.clone(),
            );
        let (
            slice30_out1,
            concat96_out1,
            expand6_out1,
            shape154_out1,
            gather138_out1,
            concat87_out1,
        ) = self.submodule14.forward(transpose73_out1.clone(), gatherelements1_out1);
        let (add43_out1, linear73_out1, unsqueeze179_out1) = self
            .submodule15
            .forward(slice30_out1, concat96_out1, expand6_out1);
        let (add47_out1, slice45_out1, slice44_out1, concat108_out1, concat109_out1) = self
            .submodule16
            .forward(
                add43_out1,
                linear73_out1.clone(),
                shape154_out1,
                transpose73_out1.clone(),
                gather138_out1.clone(),
                unsqueeze179_out1,
                mul97_out1.clone(),
                gather134_out1.clone(),
                gather135_out1.clone(),
            );
        let add54_out1 = self
            .submodule17
            .forward(
                add47_out1,
                linear73_out1.clone(),
                transpose73_out1.clone(),
                gather138_out1.clone(),
                slice45_out1.clone(),
                slice44_out1.clone(),
                concat108_out1,
                mul97_out1.clone(),
                gather134_out1.clone(),
                gather135_out1.clone(),
            );
        let (reshape123_out1, add59_out1, softmax17_out1, layernormalization43_out1) = self
            .submodule18
            .forward(
                add54_out1,
                linear73_out1,
                transpose73_out1,
                gather138_out1,
                slice45_out1,
                slice44_out1,
                concat109_out1,
            );
        let (concat148_out1, linear107_out1) = self
            .submodule19
            .forward(
                reshape123_out1,
                add59_out1,
                mul97_out1,
                gather134_out1,
                gather135_out1,
                softmax17_out1,
                layernormalization43_out1,
                concat87_out1,
            );
        (concat148_out1, linear107_out1)
    }
}
