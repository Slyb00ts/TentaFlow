// Generated from ONNX "/home/critix/repos/rust/TentaFlow/.runtime/cache/ml-training-artifacts/recog/base-deploy/rfdetr-base-b8.onnx" by burn-onnx
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
use burn_store::BurnpackStore;
use burn_store::ModuleSnapshot;


#[derive(Module, Debug)]
pub struct Submodule1<B: Backend> {
    conv2d1: Conv2d<B>,
    constant326: burn::module::Param<Tensor<B, 3>>,
    constant58: burn::module::Param<Tensor<B, 3>>,
    layernormalization1: LayerNorm<B>,
    linear1: Linear<B>,
    constant63: burn::module::Param<Tensor<B, 1>>,
    constant64: burn::module::Param<Tensor<B, 1>>,
    constant65: burn::module::Param<Tensor<B, 1>>,
    linear2: Linear<B>,
    constant67: burn::module::Param<Tensor<B, 1>>,
    layernormalization2: LayerNorm<B>,
    linear3: Linear<B>,
    constant310: burn::module::Param<Tensor<B, 1>>,
    constant311: burn::module::Param<Tensor<B, 1>>,
    constant312: burn::module::Param<Tensor<B, 1>>,
    linear4: Linear<B>,
    constant72: burn::module::Param<Tensor<B, 1>>,
    layernormalization3: LayerNorm<B>,
    linear5: Linear<B>,
    constant75: burn::module::Param<Tensor<B, 1>>,
    constant76: burn::module::Param<Tensor<B, 1>>,
    constant77: burn::module::Param<Tensor<B, 1>>,
    linear6: Linear<B>,
    constant79: burn::module::Param<Tensor<B, 1>>,
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
        let constant326: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 1, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 1, 384].into(),
        );
        let constant58: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1601, 384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [1, 1601, 384].into(),
        );
        let layernormalization1 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear1 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant64: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant65: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear2 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant67: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear3 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let constant310: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant311: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant312: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear4 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant72: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear5 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant76: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant77: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear6 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant79: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            constant326,
            constant58,
            layernormalization1,
            linear1,
            constant63,
            constant64,
            constant65,
            linear2,
            constant67,
            layernormalization2,
            linear3,
            constant310,
            constant311,
            constant312,
            linear4,
            constant72,
            layernormalization3,
            linear5,
            constant75,
            constant76,
            constant77,
            linear6,
            constant79,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        input: Tensor<B, 4>,
    ) -> (Tensor<B, 3>, Tensor<B, 1>, Tensor<B, 1>, Tensor<B, 1>) {
        let conv2d1_out1 = self.conv2d1.forward(input);
        let reshape1_out1 = conv2d1_out1.reshape([8, 384, -1]);
        let transpose1_out1 = reshape1_out1.permute([0, 2, 1]);
        let constant326_out1 = self.constant326.val();
        let concat1_out1 = burn::tensor::Tensor::cat(
            [constant326_out1, transpose1_out1].into(),
            1,
        );
        let constant58_out1 = self.constant58.val();
        let add1_out1 = concat1_out1.add(constant58_out1);
        let slice1_out1 = add1_out1.clone().slice(s![.., 0..1, ..]);
        let slice2_out1 = add1_out1.slice(s![.., 1.., ..]);
        let reshape2_out1 = slice2_out1.reshape([32, 10, 4, 10, -1]);
        let transpose2_out1 = reshape2_out1.permute([0, 2, 1, 3, 4]);
        let reshape3_out1 = transpose2_out1.reshape([128, 100, -1]);
        let tile1_out1 = slice1_out1.repeat(&[16, 1, 1]);
        let concat2_out1 = burn::tensor::Tensor::cat(
            [tile1_out1, reshape3_out1].into(),
            1,
        );
        let layernormalization1_out1 = {
            let dtype = concat2_out1.clone().dtype();
            self.layernormalization1
                .forward(concat2_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear1_out1 = self.linear1.forward(layernormalization1_out1);
        let split_tensors = linear1_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split1_out1, split1_out2, split1_out3] = split_tensors.try_into().unwrap();
        let constant63_out1 = self.constant63.val();
        let add2_out1 = (constant63_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split1_out1);
        let constant64_out1 = self.constant64.val();
        let add3_out1 = (constant64_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split1_out2);
        let reshape4_out1 = add3_out1.reshape([128, -1, 6, 64]);
        let constant65_out1 = self.constant65.val();
        let add4_out1 = (constant65_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split1_out3);
        let reshape5_out1 = add4_out1.reshape([128, -1, 6, 64]);
        let transpose3_out1 = reshape5_out1.permute([0, 2, 1, 3]);
        let reshape6_out1 = add2_out1.reshape([128, -1, 6, 64]);
        let transpose4_out1 = reshape6_out1.permute([0, 2, 1, 3]);
        let transpose5_out1 = reshape4_out1.permute([0, 2, 3, 1]);
        let matmul2_k_corrected = transpose5_out1.permute([0, 1, 3, 2]);
        let (matmul3_out1,) = {
            let q = transpose4_out1;
            let k = matmul2_k_corrected;
            let v = transpose3_out1;
            let matmul3_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul3_out1,)
        };
        let transpose6_out1 = matmul3_out1.permute([0, 2, 1, 3]);
        let reshape7_out1 = transpose6_out1.reshape([128, 101, 384]);
        let linear2_out1 = self.linear2.forward(reshape7_out1);
        let constant67_out1 = self.constant67.val();
        let mul3_out1 = linear2_out1
            .mul((constant67_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add5_out1 = mul3_out1.add(concat2_out1);
        let layernormalization2_out1 = {
            let dtype = add5_out1.clone().dtype();
            self.layernormalization2
                .forward(add5_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear3_out1 = self.linear3.forward(layernormalization2_out1);
        let constant310_out1 = self.constant310.val();
        let div1_out1 = linear3_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf1_out1 = div1_out1.erf();
        let constant311_out1 = self.constant311.val();
        let add6_out1 = erf1_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul4_out1 = linear3_out1.mul(add6_out1);
        let constant312_out1 = self.constant312.val();
        let mul5_out1 = mul4_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear4_out1 = self.linear4.forward(mul5_out1);
        let constant72_out1 = self.constant72.val();
        let mul6_out1 = linear4_out1
            .mul((constant72_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add7_out1 = mul6_out1.add(add5_out1);
        let layernormalization3_out1 = {
            let dtype = add7_out1.clone().dtype();
            self.layernormalization3
                .forward(add7_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear5_out1 = self.linear5.forward(layernormalization3_out1);
        let split_tensors = linear5_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split2_out1, split2_out2, split2_out3] = split_tensors.try_into().unwrap();
        let constant75_out1 = self.constant75.val();
        let add8_out1 = (constant75_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split2_out1);
        let constant76_out1 = self.constant76.val();
        let add9_out1 = (constant76_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split2_out2);
        let reshape8_out1 = add9_out1.reshape([128, -1, 6, 64]);
        let constant77_out1 = self.constant77.val();
        let add10_out1 = (constant77_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split2_out3);
        let reshape9_out1 = add10_out1.reshape([128, -1, 6, 64]);
        let transpose7_out1 = reshape9_out1.permute([0, 2, 1, 3]);
        let reshape10_out1 = add8_out1.reshape([128, -1, 6, 64]);
        let transpose8_out1 = reshape10_out1.permute([0, 2, 1, 3]);
        let transpose9_out1 = reshape8_out1.permute([0, 2, 3, 1]);
        let matmul8_k_corrected = transpose9_out1.permute([0, 1, 3, 2]);
        let (matmul9_out1,) = {
            let q = transpose8_out1;
            let k = matmul8_k_corrected;
            let v = transpose7_out1;
            let matmul9_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul9_out1,)
        };
        let transpose10_out1 = matmul9_out1.permute([0, 2, 1, 3]);
        let reshape11_out1 = transpose10_out1.reshape([128, 101, 384]);
        let linear6_out1 = self.linear6.forward(reshape11_out1);
        let constant79_out1 = self.constant79.val();
        let mul9_out1 = linear6_out1
            .mul((constant79_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add11_out1 = mul9_out1.add(add7_out1);
        (add11_out1, constant310_out1, constant311_out1, constant312_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule2<B: Backend> {
    layernormalization4: LayerNorm<B>,
    linear7: Linear<B>,
    linear8: Linear<B>,
    constant84: burn::module::Param<Tensor<B, 1>>,
    layernormalization5: LayerNorm<B>,
    linear9: Linear<B>,
    constant87: burn::module::Param<Tensor<B, 1>>,
    constant88: burn::module::Param<Tensor<B, 1>>,
    constant89: burn::module::Param<Tensor<B, 1>>,
    linear10: Linear<B>,
    constant91: burn::module::Param<Tensor<B, 1>>,
    layernormalization6: LayerNorm<B>,
    linear11: Linear<B>,
    linear12: Linear<B>,
    constant96: burn::module::Param<Tensor<B, 1>>,
    layernormalization7: LayerNorm<B>,
    linear13: Linear<B>,
    constant99: burn::module::Param<Tensor<B, 1>>,
    constant100: burn::module::Param<Tensor<B, 1>>,
    constant101: burn::module::Param<Tensor<B, 1>>,
    linear14: Linear<B>,
    constant103: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule2<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization4 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear7 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear8 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant84: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear9 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant88: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant89: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear10 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant91: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear11 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear12 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant96: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear13 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant100: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant101: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear14 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant103: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            layernormalization4,
            linear7,
            linear8,
            constant84,
            layernormalization5,
            linear9,
            constant87,
            constant88,
            constant89,
            linear10,
            constant91,
            layernormalization6,
            linear11,
            linear12,
            constant96,
            layernormalization7,
            linear13,
            constant99,
            constant100,
            constant101,
            linear14,
            constant103,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add11_out1: Tensor<B, 3>,
        constant310_out1: Tensor<B, 1>,
        constant311_out1: Tensor<B, 1>,
        constant312_out1: Tensor<B, 1>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let layernormalization4_out1 = {
            let dtype = add11_out1.clone().dtype();
            self.layernormalization4
                .forward(add11_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear7_out1 = self.linear7.forward(layernormalization4_out1);
        let div2_out1 = linear7_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf2_out1 = div2_out1.erf();
        let add12_out1 = erf2_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul10_out1 = linear7_out1.mul(add12_out1);
        let mul11_out1 = mul10_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear8_out1 = self.linear8.forward(mul11_out1);
        let constant84_out1 = self.constant84.val();
        let mul12_out1 = linear8_out1
            .mul((constant84_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add13_out1 = mul12_out1.add(add11_out1);
        let reshape12_out1 = add13_out1.clone().reshape([8, -1, 384]);
        let layernormalization5_out1 = {
            let dtype = reshape12_out1.dtype();
            self.layernormalization5
                .forward(reshape12_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear9_out1 = self.linear9.forward(layernormalization5_out1);
        let split_tensors = linear9_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split3_out1, split3_out2, split3_out3] = split_tensors.try_into().unwrap();
        let constant87_out1 = self.constant87.val();
        let add14_out1 = (constant87_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split3_out1);
        let constant88_out1 = self.constant88.val();
        let add15_out1 = (constant88_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split3_out2);
        let reshape13_out1 = add15_out1.reshape([8, 1616, 6, 64]);
        let constant89_out1 = self.constant89.val();
        let add16_out1 = (constant89_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split3_out3);
        let reshape14_out1 = add16_out1.reshape([8, 1616, 6, 64]);
        let transpose11_out1 = reshape14_out1.permute([0, 2, 1, 3]);
        let reshape15_out1 = add14_out1.reshape([8, 1616, 6, 64]);
        let transpose12_out1 = reshape15_out1.permute([0, 2, 1, 3]);
        let transpose13_out1 = reshape13_out1.permute([0, 2, 3, 1]);
        let matmul14_k_corrected = transpose13_out1.permute([0, 1, 3, 2]);
        let (matmul15_out1,) = {
            let q = transpose12_out1;
            let k = matmul14_k_corrected;
            let v = transpose11_out1;
            let matmul15_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul15_out1,)
        };
        let transpose14_out1 = matmul15_out1.permute([0, 2, 1, 3]);
        let reshape16_out1 = transpose14_out1.reshape([8, 1616, 384]);
        let linear10_out1 = self.linear10.forward(reshape16_out1);
        let reshape17_out1 = linear10_out1.reshape([128, 101, 384]);
        let constant91_out1 = self.constant91.val();
        let mul15_out1 = reshape17_out1
            .mul((constant91_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add17_out1 = mul15_out1.add(add13_out1.clone());
        let layernormalization6_out1 = {
            let dtype = add17_out1.clone().dtype();
            self.layernormalization6
                .forward(add17_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear11_out1 = self.linear11.forward(layernormalization6_out1);
        let div3_out1 = linear11_out1
            .clone()
            .div((constant310_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf3_out1 = div3_out1.erf();
        let add18_out1 = erf3_out1
            .add((constant311_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul16_out1 = linear11_out1.mul(add18_out1);
        let mul17_out1 = mul16_out1
            .mul((constant312_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear12_out1 = self.linear12.forward(mul17_out1);
        let constant96_out1 = self.constant96.val();
        let mul18_out1 = linear12_out1
            .mul((constant96_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add19_out1 = mul18_out1.add(add17_out1);
        let layernormalization7_out1 = {
            let dtype = add19_out1.clone().dtype();
            self.layernormalization7
                .forward(add19_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear13_out1 = self.linear13.forward(layernormalization7_out1);
        let split_tensors = linear13_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split4_out1, split4_out2, split4_out3] = split_tensors.try_into().unwrap();
        let constant99_out1 = self.constant99.val();
        let add20_out1 = (constant99_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split4_out1);
        let constant100_out1 = self.constant100.val();
        let add21_out1 = (constant100_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split4_out2);
        let reshape18_out1 = add21_out1.reshape([128, -1, 6, 64]);
        let constant101_out1 = self.constant101.val();
        let add22_out1 = (constant101_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split4_out3);
        let reshape19_out1 = add22_out1.reshape([128, -1, 6, 64]);
        let transpose15_out1 = reshape19_out1.permute([0, 2, 1, 3]);
        let reshape20_out1 = add20_out1.reshape([128, -1, 6, 64]);
        let transpose16_out1 = reshape20_out1.permute([0, 2, 1, 3]);
        let transpose17_out1 = reshape18_out1.permute([0, 2, 3, 1]);
        let matmul20_k_corrected = transpose17_out1.permute([0, 1, 3, 2]);
        let (matmul21_out1,) = {
            let q = transpose16_out1;
            let k = matmul20_k_corrected;
            let v = transpose15_out1;
            let matmul21_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul21_out1,)
        };
        let transpose18_out1 = matmul21_out1.permute([0, 2, 1, 3]);
        let reshape21_out1 = transpose18_out1.reshape([128, 101, 384]);
        let linear14_out1 = self.linear14.forward(reshape21_out1);
        let constant103_out1 = self.constant103.val();
        let mul21_out1 = linear14_out1
            .mul((constant103_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add23_out1 = mul21_out1.add(add19_out1);
        (add23_out1, add13_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule3<B: Backend> {
    layernormalization8: LayerNorm<B>,
    linear15: Linear<B>,
    linear16: Linear<B>,
    constant108: burn::module::Param<Tensor<B, 1>>,
    layernormalization9: LayerNorm<B>,
    linear17: Linear<B>,
    constant111: burn::module::Param<Tensor<B, 1>>,
    constant112: burn::module::Param<Tensor<B, 1>>,
    constant113: burn::module::Param<Tensor<B, 1>>,
    linear18: Linear<B>,
    constant115: burn::module::Param<Tensor<B, 1>>,
    layernormalization10: LayerNorm<B>,
    linear19: Linear<B>,
    linear20: Linear<B>,
    constant120: burn::module::Param<Tensor<B, 1>>,
    layernormalization11: LayerNorm<B>,
    linear21: Linear<B>,
    constant123: burn::module::Param<Tensor<B, 1>>,
    constant124: burn::module::Param<Tensor<B, 1>>,
    constant125: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule3<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization8 = LayerNormConfig::new(384)
            .with_epsilon(0.0000009999999974752427f64)
            .with_bias(true)
            .init(device);
        let linear15 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear16 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant108: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear17 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant112: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant113: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear18 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant115: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear19 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear20 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant120: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear21 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant124: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant125: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            linear15,
            linear16,
            constant108,
            layernormalization9,
            linear17,
            constant111,
            constant112,
            constant113,
            linear18,
            constant115,
            layernormalization10,
            linear19,
            linear20,
            constant120,
            layernormalization11,
            linear21,
            constant123,
            constant124,
            constant125,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add23_out1: Tensor<B, 3>,
        constant310_out1: Tensor<B, 1>,
        constant311_out1: Tensor<B, 1>,
        constant312_out1: Tensor<B, 1>,
    ) -> (Tensor<B, 4>, Tensor<B, 3>) {
        let layernormalization8_out1 = {
            let dtype = add23_out1.clone().dtype();
            self.layernormalization8
                .forward(add23_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear15_out1 = self.linear15.forward(layernormalization8_out1);
        let div4_out1 = linear15_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf4_out1 = div4_out1.erf();
        let add24_out1 = erf4_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul22_out1 = linear15_out1.mul(add24_out1);
        let mul23_out1 = mul22_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear16_out1 = self.linear16.forward(mul23_out1);
        let constant108_out1 = self.constant108.val();
        let mul24_out1 = linear16_out1
            .mul((constant108_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add25_out1 = mul24_out1.add(add23_out1);
        let layernormalization9_out1 = {
            let dtype = add25_out1.clone().dtype();
            self.layernormalization9
                .forward(add25_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear17_out1 = self.linear17.forward(layernormalization9_out1);
        let split_tensors = linear17_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split5_out1, split5_out2, split5_out3] = split_tensors.try_into().unwrap();
        let constant111_out1 = self.constant111.val();
        let add26_out1 = (constant111_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split5_out1);
        let constant112_out1 = self.constant112.val();
        let add27_out1 = (constant112_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split5_out2);
        let reshape22_out1 = add27_out1.reshape([128, -1, 6, 64]);
        let constant113_out1 = self.constant113.val();
        let add28_out1 = (constant113_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split5_out3);
        let reshape23_out1 = add28_out1.reshape([128, -1, 6, 64]);
        let transpose19_out1 = reshape23_out1.permute([0, 2, 1, 3]);
        let reshape24_out1 = add26_out1.reshape([128, -1, 6, 64]);
        let transpose20_out1 = reshape24_out1.permute([0, 2, 1, 3]);
        let transpose21_out1 = reshape22_out1.permute([0, 2, 3, 1]);
        let matmul26_k_corrected = transpose21_out1.permute([0, 1, 3, 2]);
        let (matmul27_out1,) = {
            let q = transpose20_out1;
            let k = matmul26_k_corrected;
            let v = transpose19_out1;
            let matmul27_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul27_out1,)
        };
        let transpose22_out1 = matmul27_out1.permute([0, 2, 1, 3]);
        let reshape25_out1 = transpose22_out1.reshape([128, 101, 384]);
        let linear18_out1 = self.linear18.forward(reshape25_out1);
        let constant115_out1 = self.constant115.val();
        let mul27_out1 = linear18_out1
            .mul((constant115_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add29_out1 = mul27_out1.add(add25_out1);
        let layernormalization10_out1 = {
            let dtype = add29_out1.clone().dtype();
            self.layernormalization10
                .forward(add29_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear19_out1 = self.linear19.forward(layernormalization10_out1);
        let div5_out1 = linear19_out1
            .clone()
            .div((constant310_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf5_out1 = div5_out1.erf();
        let add30_out1 = erf5_out1
            .add((constant311_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul28_out1 = linear19_out1.mul(add30_out1);
        let mul29_out1 = mul28_out1
            .mul((constant312_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear20_out1 = self.linear20.forward(mul29_out1);
        let constant120_out1 = self.constant120.val();
        let mul30_out1 = linear20_out1
            .mul((constant120_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add31_out1 = mul30_out1.add(add29_out1);
        let reshape26_out1 = add31_out1.clone().reshape([8, -1, 384]);
        let layernormalization11_out1 = {
            let dtype = reshape26_out1.dtype();
            self.layernormalization11
                .forward(reshape26_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear21_out1 = self.linear21.forward(layernormalization11_out1);
        let split_tensors = linear21_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split6_out1, split6_out2, split6_out3] = split_tensors.try_into().unwrap();
        let constant123_out1 = self.constant123.val();
        let add32_out1 = (constant123_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split6_out1);
        let constant124_out1 = self.constant124.val();
        let add33_out1 = (constant124_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split6_out2);
        let reshape27_out1 = add33_out1.reshape([8, 1616, 6, 64]);
        let constant125_out1 = self.constant125.val();
        let add34_out1 = (constant125_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split6_out3);
        let reshape28_out1 = add34_out1.reshape([8, 1616, 6, 64]);
        let transpose23_out1 = reshape28_out1.permute([0, 2, 1, 3]);
        let reshape29_out1 = add32_out1.reshape([8, 1616, 6, 64]);
        let transpose24_out1 = reshape29_out1.permute([0, 2, 1, 3]);
        let transpose25_out1 = reshape27_out1.permute([0, 2, 3, 1]);
        let matmul32_k_corrected = transpose25_out1.permute([0, 1, 3, 2]);
        let (matmul33_out1,) = {
            let q = transpose24_out1;
            let k = matmul32_k_corrected;
            let v = transpose23_out1;
            let matmul33_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul33_out1,)
        };
        let transpose26_out1 = matmul33_out1.permute([0, 2, 1, 3]);
        (transpose26_out1, add31_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule4<B: Backend> {
    linear22: Linear<B>,
    constant127: burn::module::Param<Tensor<B, 1>>,
    layernormalization12: LayerNorm<B>,
    linear23: Linear<B>,
    linear24: Linear<B>,
    constant132: burn::module::Param<Tensor<B, 1>>,
    layernormalization13: LayerNorm<B>,
    linear25: Linear<B>,
    constant135: burn::module::Param<Tensor<B, 1>>,
    constant136: burn::module::Param<Tensor<B, 1>>,
    constant137: burn::module::Param<Tensor<B, 1>>,
    linear26: Linear<B>,
    constant139: burn::module::Param<Tensor<B, 1>>,
    layernormalization14: LayerNorm<B>,
    linear27: Linear<B>,
    linear28: Linear<B>,
    constant144: burn::module::Param<Tensor<B, 1>>,
    layernormalization15: LayerNorm<B>,
    linear29: Linear<B>,
    constant147: burn::module::Param<Tensor<B, 1>>,
    constant148: burn::module::Param<Tensor<B, 1>>,
    constant149: burn::module::Param<Tensor<B, 1>>,
    linear30: Linear<B>,
    constant151: burn::module::Param<Tensor<B, 1>>,
    layernormalization16: LayerNorm<B>,
    linear31: Linear<B>,
    linear32: Linear<B>,
    constant156: burn::module::Param<Tensor<B, 1>>,
    layernormalization17: LayerNorm<B>,
    linear33: Linear<B>,
    constant159: burn::module::Param<Tensor<B, 1>>,
    constant160: burn::module::Param<Tensor<B, 1>>,
    constant161: burn::module::Param<Tensor<B, 1>>,
    linear34: Linear<B>,
    constant163: burn::module::Param<Tensor<B, 1>>,
    layernormalization18: LayerNorm<B>,
    linear35: Linear<B>,
    linear36: Linear<B>,
    constant168: burn::module::Param<Tensor<B, 1>>,
    layernormalization19: LayerNorm<B>,
    linear37: Linear<B>,
    constant171: burn::module::Param<Tensor<B, 1>>,
    constant172: burn::module::Param<Tensor<B, 1>>,
    constant173: burn::module::Param<Tensor<B, 1>>,
    linear38: Linear<B>,
    constant175: burn::module::Param<Tensor<B, 1>>,
    layernormalization20: LayerNorm<B>,
    linear39: Linear<B>,
    linear40: Linear<B>,
    constant180: burn::module::Param<Tensor<B, 1>>,
    layernormalization21: LayerNorm<B>,
    linear41: Linear<B>,
    constant183: burn::module::Param<Tensor<B, 1>>,
    constant184: burn::module::Param<Tensor<B, 1>>,
    constant185: burn::module::Param<Tensor<B, 1>>,
    linear42: Linear<B>,
    constant187: burn::module::Param<Tensor<B, 1>>,
    layernormalization22: LayerNorm<B>,
    linear43: Linear<B>,
    linear44: Linear<B>,
    constant192: burn::module::Param<Tensor<B, 1>>,
    layernormalization23: LayerNorm<B>,
    layernormalization24: LayerNorm<B>,
    layernormalization25: LayerNorm<B>,
    layernormalization26: LayerNorm<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule4<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let linear22 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant127: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear23 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear24 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant132: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear25 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant136: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant137: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear26 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant139: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear27 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear28 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant144: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear29 = LinearConfig::new(384, 1152).with_bias(false).init(device);
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
        let constant148: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant149: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear30 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant151: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear31 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear32 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant156: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear33 = LinearConfig::new(384, 1152).with_bias(false).init(device);
        let constant159: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant160: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant161: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear34 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant163: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear35 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear36 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant168: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear37 = LinearConfig::new(384, 1152).with_bias(false).init(device);
        let constant171: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant172: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant173: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear38 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant175: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear39 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear40 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant180: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear41 = LinearConfig::new(384, 1152).with_bias(false).init(device);
        let constant183: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant184: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let constant185: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([384], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [384].into(),
        );
        let linear42 = LinearConfig::new(384, 384).with_bias(true).init(device);
        let constant187: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear43 = LinearConfig::new(384, 1536).with_bias(true).init(device);
        let linear44 = LinearConfig::new(1536, 384).with_bias(true).init(device);
        let constant192: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
            linear22,
            constant127,
            layernormalization12,
            linear23,
            linear24,
            constant132,
            layernormalization13,
            linear25,
            constant135,
            constant136,
            constant137,
            linear26,
            constant139,
            layernormalization14,
            linear27,
            linear28,
            constant144,
            layernormalization15,
            linear29,
            constant147,
            constant148,
            constant149,
            linear30,
            constant151,
            layernormalization16,
            linear31,
            linear32,
            constant156,
            layernormalization17,
            linear33,
            constant159,
            constant160,
            constant161,
            linear34,
            constant163,
            layernormalization18,
            linear35,
            linear36,
            constant168,
            layernormalization19,
            linear37,
            constant171,
            constant172,
            constant173,
            linear38,
            constant175,
            layernormalization20,
            linear39,
            linear40,
            constant180,
            layernormalization21,
            linear41,
            constant183,
            constant184,
            constant185,
            linear42,
            constant187,
            layernormalization22,
            linear43,
            linear44,
            constant192,
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
        transpose26_out1: Tensor<B, 4>,
        add31_out1: Tensor<B, 3>,
        constant310_out1: Tensor<B, 1>,
        constant311_out1: Tensor<B, 1>,
        constant312_out1: Tensor<B, 1>,
        add13_out1: Tensor<B, 3>,
    ) -> Tensor<B, 4> {
        let reshape30_out1 = transpose26_out1.reshape([8, 1616, 384]);
        let linear22_out1 = self.linear22.forward(reshape30_out1);
        let reshape31_out1 = linear22_out1.reshape([128, 101, 384]);
        let constant127_out1 = self.constant127.val();
        let mul33_out1 = reshape31_out1
            .mul((constant127_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add35_out1 = mul33_out1.add(add31_out1.clone());
        let layernormalization12_out1 = {
            let dtype = add35_out1.clone().dtype();
            self.layernormalization12
                .forward(add35_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear23_out1 = self.linear23.forward(layernormalization12_out1);
        let div6_out1 = linear23_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf6_out1 = div6_out1.erf();
        let add36_out1 = erf6_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul34_out1 = linear23_out1.mul(add36_out1);
        let mul35_out1 = mul34_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear24_out1 = self.linear24.forward(mul35_out1);
        let constant132_out1 = self.constant132.val();
        let mul36_out1 = linear24_out1
            .mul((constant132_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add37_out1 = mul36_out1.add(add35_out1);
        let layernormalization13_out1 = {
            let dtype = add37_out1.clone().dtype();
            self.layernormalization13
                .forward(add37_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear25_out1 = self.linear25.forward(layernormalization13_out1);
        let split_tensors = linear25_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split7_out1, split7_out2, split7_out3] = split_tensors.try_into().unwrap();
        let constant135_out1 = self.constant135.val();
        let add38_out1 = (constant135_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split7_out1);
        let constant136_out1 = self.constant136.val();
        let add39_out1 = (constant136_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split7_out2);
        let reshape32_out1 = add39_out1.reshape([128, -1, 6, 64]);
        let constant137_out1 = self.constant137.val();
        let add40_out1 = (constant137_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split7_out3);
        let reshape33_out1 = add40_out1.reshape([128, -1, 6, 64]);
        let transpose27_out1 = reshape33_out1.permute([0, 2, 1, 3]);
        let reshape34_out1 = add38_out1.reshape([128, -1, 6, 64]);
        let transpose28_out1 = reshape34_out1.permute([0, 2, 1, 3]);
        let transpose29_out1 = reshape32_out1.permute([0, 2, 3, 1]);
        let matmul38_k_corrected = transpose29_out1.permute([0, 1, 3, 2]);
        let (matmul39_out1,) = {
            let q = transpose28_out1;
            let k = matmul38_k_corrected;
            let v = transpose27_out1;
            let matmul39_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul39_out1,)
        };
        let transpose30_out1 = matmul39_out1.permute([0, 2, 1, 3]);
        let reshape35_out1 = transpose30_out1.reshape([128, 101, 384]);
        let linear26_out1 = self.linear26.forward(reshape35_out1);
        let constant139_out1 = self.constant139.val();
        let mul39_out1 = linear26_out1
            .mul((constant139_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add41_out1 = mul39_out1.add(add37_out1);
        let layernormalization14_out1 = {
            let dtype = add41_out1.clone().dtype();
            self.layernormalization14
                .forward(add41_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear27_out1 = self.linear27.forward(layernormalization14_out1);
        let div7_out1 = linear27_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf7_out1 = div7_out1.erf();
        let add42_out1 = erf7_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul40_out1 = linear27_out1.mul(add42_out1);
        let mul41_out1 = mul40_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear28_out1 = self.linear28.forward(mul41_out1);
        let constant144_out1 = self.constant144.val();
        let mul42_out1 = linear28_out1
            .mul((constant144_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add43_out1 = mul42_out1.add(add41_out1);
        let layernormalization15_out1 = {
            let dtype = add43_out1.clone().dtype();
            self.layernormalization15
                .forward(add43_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear29_out1 = self.linear29.forward(layernormalization15_out1);
        let split_tensors = linear29_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split8_out1, split8_out2, split8_out3] = split_tensors.try_into().unwrap();
        let constant147_out1 = self.constant147.val();
        let add44_out1 = (constant147_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split8_out1);
        let constant148_out1 = self.constant148.val();
        let add45_out1 = (constant148_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split8_out2);
        let reshape36_out1 = add45_out1.reshape([128, -1, 6, 64]);
        let constant149_out1 = self.constant149.val();
        let add46_out1 = (constant149_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split8_out3);
        let reshape37_out1 = add46_out1.reshape([128, -1, 6, 64]);
        let transpose31_out1 = reshape37_out1.permute([0, 2, 1, 3]);
        let reshape38_out1 = add44_out1.reshape([128, -1, 6, 64]);
        let transpose32_out1 = reshape38_out1.permute([0, 2, 1, 3]);
        let transpose33_out1 = reshape36_out1.permute([0, 2, 3, 1]);
        let matmul44_k_corrected = transpose33_out1.permute([0, 1, 3, 2]);
        let (matmul45_out1,) = {
            let q = transpose32_out1;
            let k = matmul44_k_corrected;
            let v = transpose31_out1;
            let matmul45_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul45_out1,)
        };
        let transpose34_out1 = matmul45_out1.permute([0, 2, 1, 3]);
        let reshape39_out1 = transpose34_out1.reshape([128, 101, 384]);
        let linear30_out1 = self.linear30.forward(reshape39_out1);
        let constant151_out1 = self.constant151.val();
        let mul45_out1 = linear30_out1
            .mul((constant151_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add47_out1 = mul45_out1.add(add43_out1);
        let layernormalization16_out1 = {
            let dtype = add47_out1.clone().dtype();
            self.layernormalization16
                .forward(add47_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear31_out1 = self.linear31.forward(layernormalization16_out1);
        let div8_out1 = linear31_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf8_out1 = div8_out1.erf();
        let add48_out1 = erf8_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul46_out1 = linear31_out1.mul(add48_out1);
        let mul47_out1 = mul46_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear32_out1 = self.linear32.forward(mul47_out1);
        let constant156_out1 = self.constant156.val();
        let mul48_out1 = linear32_out1
            .mul((constant156_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add49_out1 = mul48_out1.add(add47_out1);
        let reshape40_out1 = add49_out1.clone().reshape([8, -1, 384]);
        let layernormalization17_out1 = {
            let dtype = reshape40_out1.dtype();
            self.layernormalization17
                .forward(reshape40_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear33_out1 = self.linear33.forward(layernormalization17_out1);
        let split_tensors = linear33_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split9_out1, split9_out2, split9_out3] = split_tensors.try_into().unwrap();
        let constant159_out1 = self.constant159.val();
        let add50_out1 = (constant159_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split9_out1);
        let constant160_out1 = self.constant160.val();
        let add51_out1 = (constant160_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split9_out2);
        let reshape41_out1 = add51_out1.reshape([8, 1616, 6, 64]);
        let constant161_out1 = self.constant161.val();
        let add52_out1 = (constant161_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split9_out3);
        let reshape42_out1 = add52_out1.reshape([8, 1616, 6, 64]);
        let transpose35_out1 = reshape42_out1.permute([0, 2, 1, 3]);
        let reshape43_out1 = add50_out1.reshape([8, 1616, 6, 64]);
        let transpose36_out1 = reshape43_out1.permute([0, 2, 1, 3]);
        let transpose37_out1 = reshape41_out1.permute([0, 2, 3, 1]);
        let matmul50_k_corrected = transpose37_out1.permute([0, 1, 3, 2]);
        let (matmul51_out1,) = {
            let q = transpose36_out1;
            let k = matmul50_k_corrected;
            let v = transpose35_out1;
            let matmul51_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul51_out1,)
        };
        let transpose38_out1 = matmul51_out1.permute([0, 2, 1, 3]);
        let reshape44_out1 = transpose38_out1.reshape([8, 1616, 384]);
        let linear34_out1 = self.linear34.forward(reshape44_out1);
        let reshape45_out1 = linear34_out1.reshape([128, 101, 384]);
        let constant163_out1 = self.constant163.val();
        let mul51_out1 = reshape45_out1
            .mul((constant163_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add53_out1 = mul51_out1.add(add49_out1.clone());
        let layernormalization18_out1 = {
            let dtype = add53_out1.clone().dtype();
            self.layernormalization18
                .forward(add53_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear35_out1 = self.linear35.forward(layernormalization18_out1);
        let div9_out1 = linear35_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf9_out1 = div9_out1.erf();
        let add54_out1 = erf9_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul52_out1 = linear35_out1.mul(add54_out1);
        let mul53_out1 = mul52_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear36_out1 = self.linear36.forward(mul53_out1);
        let constant168_out1 = self.constant168.val();
        let mul54_out1 = linear36_out1
            .mul((constant168_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add55_out1 = mul54_out1.add(add53_out1);
        let layernormalization19_out1 = {
            let dtype = add55_out1.clone().dtype();
            self.layernormalization19
                .forward(add55_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear37_out1 = self.linear37.forward(layernormalization19_out1);
        let split_tensors = linear37_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split10_out1, split10_out2, split10_out3] = split_tensors
            .try_into()
            .unwrap();
        let constant171_out1 = self.constant171.val();
        let add56_out1 = (constant171_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split10_out1);
        let constant172_out1 = self.constant172.val();
        let add57_out1 = (constant172_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split10_out2);
        let reshape46_out1 = add57_out1.reshape([128, -1, 6, 64]);
        let constant173_out1 = self.constant173.val();
        let add58_out1 = (constant173_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split10_out3);
        let reshape47_out1 = add58_out1.reshape([128, -1, 6, 64]);
        let transpose39_out1 = reshape47_out1.permute([0, 2, 1, 3]);
        let reshape48_out1 = add56_out1.reshape([128, -1, 6, 64]);
        let transpose40_out1 = reshape48_out1.permute([0, 2, 1, 3]);
        let transpose41_out1 = reshape46_out1.permute([0, 2, 3, 1]);
        let matmul56_k_corrected = transpose41_out1.permute([0, 1, 3, 2]);
        let (matmul57_out1,) = {
            let q = transpose40_out1;
            let k = matmul56_k_corrected;
            let v = transpose39_out1;
            let matmul57_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul57_out1,)
        };
        let transpose42_out1 = matmul57_out1.permute([0, 2, 1, 3]);
        let reshape49_out1 = transpose42_out1.reshape([128, 101, 384]);
        let linear38_out1 = self.linear38.forward(reshape49_out1);
        let constant175_out1 = self.constant175.val();
        let mul57_out1 = linear38_out1
            .mul((constant175_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add59_out1 = mul57_out1.add(add55_out1);
        let layernormalization20_out1 = {
            let dtype = add59_out1.clone().dtype();
            self.layernormalization20
                .forward(add59_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear39_out1 = self.linear39.forward(layernormalization20_out1);
        let div10_out1 = linear39_out1
            .clone()
            .div((constant310_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf10_out1 = div10_out1.erf();
        let add60_out1 = erf10_out1
            .add((constant311_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul58_out1 = linear39_out1.mul(add60_out1);
        let mul59_out1 = mul58_out1
            .mul((constant312_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear40_out1 = self.linear40.forward(mul59_out1);
        let constant180_out1 = self.constant180.val();
        let mul60_out1 = linear40_out1
            .mul((constant180_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add61_out1 = mul60_out1.add(add59_out1);
        let layernormalization21_out1 = {
            let dtype = add61_out1.clone().dtype();
            self.layernormalization21
                .forward(add61_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear41_out1 = self.linear41.forward(layernormalization21_out1);
        let split_tensors = linear41_out1.split_with_sizes([384, 384, 384].into(), 2);
        let [split11_out1, split11_out2, split11_out3] = split_tensors
            .try_into()
            .unwrap();
        let constant183_out1 = self.constant183.val();
        let add62_out1 = (constant183_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split11_out1);
        let constant184_out1 = self.constant184.val();
        let add63_out1 = (constant184_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split11_out2);
        let reshape50_out1 = add63_out1.reshape([128, -1, 6, 64]);
        let constant185_out1 = self.constant185.val();
        let add64_out1 = (constant185_out1)
            .unsqueeze_dims(&[0isize, 1isize])
            .add(split11_out3);
        let reshape51_out1 = add64_out1.reshape([128, -1, 6, 64]);
        let transpose43_out1 = reshape51_out1.permute([0, 2, 1, 3]);
        let reshape52_out1 = add62_out1.reshape([128, -1, 6, 64]);
        let transpose44_out1 = reshape52_out1.permute([0, 2, 1, 3]);
        let transpose45_out1 = reshape50_out1.permute([0, 2, 3, 1]);
        let matmul62_k_corrected = transpose45_out1.permute([0, 1, 3, 2]);
        let (matmul63_out1,) = {
            let q = transpose44_out1;
            let k = matmul62_k_corrected;
            let v = transpose43_out1;
            let matmul63_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1249999925494194f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul63_out1,)
        };
        let transpose46_out1 = matmul63_out1.permute([0, 2, 1, 3]);
        let reshape53_out1 = transpose46_out1.reshape([128, 101, 384]);
        let linear42_out1 = self.linear42.forward(reshape53_out1);
        let constant187_out1 = self.constant187.val();
        let mul63_out1 = linear42_out1
            .mul((constant187_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add65_out1 = mul63_out1.add(add61_out1);
        let layernormalization22_out1 = {
            let dtype = add65_out1.clone().dtype();
            self.layernormalization22
                .forward(add65_out1.clone().cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear43_out1 = self.linear43.forward(layernormalization22_out1);
        let div11_out1 = linear43_out1
            .clone()
            .div((constant310_out1).unsqueeze_dims(&[0isize, 1isize]));
        let erf11_out1 = div11_out1.erf();
        let add66_out1 = erf11_out1
            .add((constant311_out1).unsqueeze_dims(&[0isize, 1isize]));
        let mul64_out1 = linear43_out1.mul(add66_out1);
        let mul65_out1 = mul64_out1
            .mul((constant312_out1).unsqueeze_dims(&[0isize, 1isize]));
        let linear44_out1 = self.linear44.forward(mul65_out1);
        let constant192_out1 = self.constant192.val();
        let mul66_out1 = linear44_out1
            .mul((constant192_out1).unsqueeze_dims(&[0isize, 1isize]));
        let add67_out1 = mul66_out1.add(add65_out1);
        let layernormalization23_out1 = {
            let dtype = add13_out1.dtype();
            self.layernormalization23
                .forward(add13_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice3_out1 = layernormalization23_out1.slice(s![.., 1.., ..]);
        let reshape54_out1 = slice3_out1.reshape([32, 4, 10, 10, 384]);
        let transpose47_out1 = reshape54_out1.permute([0, 2, 1, 3, 4]);
        let reshape55_out1 = transpose47_out1.reshape([8, 40, 40, -1]);
        let transpose48_out1 = reshape55_out1.permute([0, 3, 1, 2]);
        let layernormalization24_out1 = {
            let dtype = add31_out1.dtype();
            self.layernormalization24
                .forward(add31_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice4_out1 = layernormalization24_out1.slice(s![.., 1.., ..]);
        let reshape56_out1 = slice4_out1.reshape([32, 4, 10, 10, 384]);
        let transpose49_out1 = reshape56_out1.permute([0, 2, 1, 3, 4]);
        let reshape57_out1 = transpose49_out1.reshape([8, 40, 40, -1]);
        let transpose50_out1 = reshape57_out1.permute([0, 3, 1, 2]);
        let layernormalization25_out1 = {
            let dtype = add49_out1.dtype();
            self.layernormalization25
                .forward(add49_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice5_out1 = layernormalization25_out1.slice(s![.., 1.., ..]);
        let reshape58_out1 = slice5_out1.reshape([32, 4, 10, 10, 384]);
        let transpose51_out1 = reshape58_out1.permute([0, 2, 1, 3, 4]);
        let reshape59_out1 = transpose51_out1.reshape([8, 40, 40, -1]);
        let transpose52_out1 = reshape59_out1.permute([0, 3, 1, 2]);
        let layernormalization26_out1 = {
            let dtype = add67_out1.dtype();
            self.layernormalization26
                .forward(add67_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let slice6_out1 = layernormalization26_out1.slice(s![.., 1.., ..]);
        let reshape60_out1 = slice6_out1.reshape([32, 4, 10, 10, 384]);
        let transpose53_out1 = reshape60_out1.permute([0, 2, 1, 3, 4]);
        let reshape61_out1 = transpose53_out1.reshape([8, 40, 40, -1]);
        let transpose54_out1 = reshape61_out1.permute([0, 3, 1, 2]);
        let concat3_out1 = burn::tensor::Tensor::cat(
            [transpose48_out1, transpose50_out1, transpose52_out1, transpose54_out1]
                .into(),
            1,
        );
        concat3_out1
    }
}
#[derive(Module, Debug)]
pub struct Submodule5<B: Backend> {
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
    constant355: burn::module::Param<Tensor<B, 3, Bool>>,
    linear45: Linear<B>,
    layernormalization36: LayerNorm<B>,
    linear46: Linear<B>,
    linear47: Linear<B>,
    linear48: Linear<B>,
    linear49: Linear<B>,
    constant356: burn::module::Param<Tensor<B, 3>>,
    constant357: burn::module::Param<Tensor<B, 3>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule5<B> {
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
        let constant355: burn::module::Param<Tensor<B, 3, Bool>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
                Bool,
            >::empty(
                [8, 1600, 1],
                (device, burn::tensor::DType::Bool(burn::tensor::BoolStore::Native)),
            ),
            device.clone(),
            false,
            [8, 1600, 1].into(),
        );
        let linear45 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization36 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear46 = LinearConfig::new(256, 18).with_bias(true).init(device);
        let linear47 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear48 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear49 = LinearConfig::new(256, 4).with_bias(true).init(device);
        let constant356: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 1600, 2], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 1600, 2].into(),
        );
        let constant357: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 1600, 2], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 1600, 2].into(),
        );
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
            constant355,
            linear45,
            layernormalization36,
            linear46,
            linear47,
            linear48,
            linear49,
            constant356,
            constant357,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, concat3_out1: Tensor<B, 4>) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let conv2d2_out1 = self.conv2d2.forward(concat3_out1);
        let transpose55_out1 = conv2d2_out1.permute([0, 2, 3, 1]);
        let layernormalization27_out1 = {
            let dtype = transpose55_out1.dtype();
            self.layernormalization27
                .forward(transpose55_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose56_out1 = layernormalization27_out1.permute([0, 3, 1, 2]);
        let sigmoid1_out1 = burn::tensor::activation::sigmoid(transpose56_out1.clone());
        let mul67_out1 = transpose56_out1.mul(sigmoid1_out1);
        let split_tensors = mul67_out1.split_with_sizes([128, 128].into(), 1);
        let [split12_out1, split12_out2] = split_tensors.try_into().unwrap();
        let conv2d3_out1 = self.conv2d3.forward(split12_out2.clone());
        let transpose57_out1 = conv2d3_out1.permute([0, 2, 3, 1]);
        let layernormalization28_out1 = {
            let dtype = transpose57_out1.dtype();
            self.layernormalization28
                .forward(transpose57_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose58_out1 = layernormalization28_out1.permute([0, 3, 1, 2]);
        let sigmoid2_out1 = burn::tensor::activation::sigmoid(transpose58_out1.clone());
        let mul68_out1 = transpose58_out1.mul(sigmoid2_out1);
        let conv2d4_out1 = self.conv2d4.forward(mul68_out1);
        let transpose59_out1 = conv2d4_out1.permute([0, 2, 3, 1]);
        let layernormalization29_out1 = {
            let dtype = transpose59_out1.dtype();
            self.layernormalization29
                .forward(transpose59_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose60_out1 = layernormalization29_out1.permute([0, 3, 1, 2]);
        let sigmoid3_out1 = burn::tensor::activation::sigmoid(transpose60_out1.clone());
        let mul69_out1 = transpose60_out1.mul(sigmoid3_out1);
        let conv2d5_out1 = self.conv2d5.forward(mul69_out1.clone());
        let transpose61_out1 = conv2d5_out1.permute([0, 2, 3, 1]);
        let layernormalization30_out1 = {
            let dtype = transpose61_out1.dtype();
            self.layernormalization30
                .forward(transpose61_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose62_out1 = layernormalization30_out1.permute([0, 3, 1, 2]);
        let sigmoid4_out1 = burn::tensor::activation::sigmoid(transpose62_out1.clone());
        let mul70_out1 = transpose62_out1.mul(sigmoid4_out1);
        let conv2d6_out1 = self.conv2d6.forward(mul70_out1);
        let transpose63_out1 = conv2d6_out1.permute([0, 2, 3, 1]);
        let layernormalization31_out1 = {
            let dtype = transpose63_out1.dtype();
            self.layernormalization31
                .forward(transpose63_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose64_out1 = layernormalization31_out1.permute([0, 3, 1, 2]);
        let sigmoid5_out1 = burn::tensor::activation::sigmoid(transpose64_out1.clone());
        let mul71_out1 = transpose64_out1.mul(sigmoid5_out1);
        let conv2d7_out1 = self.conv2d7.forward(mul71_out1.clone());
        let transpose65_out1 = conv2d7_out1.permute([0, 2, 3, 1]);
        let layernormalization32_out1 = {
            let dtype = transpose65_out1.dtype();
            self.layernormalization32
                .forward(transpose65_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose66_out1 = layernormalization32_out1.permute([0, 3, 1, 2]);
        let sigmoid6_out1 = burn::tensor::activation::sigmoid(transpose66_out1.clone());
        let mul72_out1 = transpose66_out1.mul(sigmoid6_out1);
        let conv2d8_out1 = self.conv2d8.forward(mul72_out1);
        let transpose67_out1 = conv2d8_out1.permute([0, 2, 3, 1]);
        let layernormalization33_out1 = {
            let dtype = transpose67_out1.dtype();
            self.layernormalization33
                .forward(transpose67_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose68_out1 = layernormalization33_out1.permute([0, 3, 1, 2]);
        let sigmoid7_out1 = burn::tensor::activation::sigmoid(transpose68_out1.clone());
        let mul73_out1 = transpose68_out1.mul(sigmoid7_out1);
        let concat4_out1 = burn::tensor::Tensor::cat(
            [split12_out1, split12_out2, mul69_out1, mul71_out1, mul73_out1].into(),
            1,
        );
        let conv2d9_out1 = self.conv2d9.forward(concat4_out1);
        let transpose69_out1 = conv2d9_out1.permute([0, 2, 3, 1]);
        let layernormalization34_out1 = {
            let dtype = transpose69_out1.dtype();
            self.layernormalization34
                .forward(transpose69_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose70_out1 = layernormalization34_out1.permute([0, 3, 1, 2]);
        let sigmoid8_out1 = burn::tensor::activation::sigmoid(transpose70_out1.clone());
        let mul74_out1 = transpose70_out1.mul(sigmoid8_out1);
        let transpose71_out1 = mul74_out1.permute([0, 2, 3, 1]);
        let layernormalization35_out1 = {
            let dtype = transpose71_out1.dtype();
            self.layernormalization35
                .forward(transpose71_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let transpose72_out1 = layernormalization35_out1.permute([0, 3, 1, 2]);
        let reshape62_out1 = transpose72_out1.reshape([8, 256, -1]);
        let transpose73_out1 = reshape62_out1.permute([0, 2, 1]);
        let constant315_out1 = 0f32;
        let constant355_out1 = self.constant355.val();
        let where1_out1 = transpose73_out1
            .clone()
            .mask_fill(constant355_out1, constant315_out1);
        let linear45_out1 = self.linear45.forward(where1_out1);
        let layernormalization36_out1 = {
            let dtype = linear45_out1.dtype();
            self.layernormalization36
                .forward(linear45_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear46_out1 = self.linear46.forward(layernormalization36_out1.clone());
        let linear47_out1 = self.linear47.forward(layernormalization36_out1);
        let relu1_out1 = burn::tensor::activation::relu(linear47_out1);
        let linear48_out1 = self.linear48.forward(relu1_out1);
        let relu2_out1 = burn::tensor::activation::relu(linear48_out1);
        let linear49_out1 = self.linear49.forward(relu2_out1);
        let slice7_out1 = linear49_out1.clone().slice(s![.., .., 0..2]);
        let constant356_out1 = self.constant356.val();
        let mul75_out1 = slice7_out1.mul(constant356_out1.clone());
        let constant357_out1 = self.constant357.val();
        let add68_out1 = mul75_out1.add(constant357_out1);
        let slice8_out1 = linear49_out1.slice(s![.., .., 2..]);
        let exp1_out1 = slice8_out1.exp();
        let mul76_out1 = exp1_out1.mul(constant356_out1);
        let concat5_out1 = burn::tensor::Tensor::cat([add68_out1, mul76_out1].into(), 2);
        let reducemax1_out1 = {
            linear46_out1.max_dim(2usize).squeeze_dims::<2usize>(&[2])
        };
        let (topk1_out1, __topk_indices_raw) = reducemax1_out1.topk_with_indices(300, 1);
        let topk1_out2 = __topk_indices_raw.cast(burn::tensor::DType::I64);
        let unsqueeze1_out1: Tensor<B, 3, Int> = topk1_out2.unsqueeze_dims::<3>(&[-1]);
        let tile2_out1 = unsqueeze1_out1.repeat(&[1, 1, 4]);
        let gatherelements1_out1 = concat5_out1.gather(1, tile2_out1);
        (gatherelements1_out1, transpose73_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule6<B: Backend> {
    constant359: burn::module::Param<Tensor<B, 3>>,
    constant360: burn::module::Param<Tensor<B, 3>>,
    constant318: burn::module::Param<Tensor<B, 1>>,
    constant319: burn::module::Param<Tensor<B, 1>>,
    linear50: Linear<B>,
    linear51: Linear<B>,
    constant358: burn::module::Param<Tensor<B, 3>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule6<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant359: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 300, 2], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 300, 2].into(),
        );
        let constant360: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 300, 2], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 300, 2].into(),
        );
        let constant318: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let constant319: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([128], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [128].into(),
        );
        let linear50 = LinearConfig::new(512, 256).with_bias(true).init(device);
        let linear51 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let constant358: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([8, 300, 256], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 300, 256].into(),
        );
        Self {
            constant359,
            constant360,
            constant318,
            constant319,
            linear50,
            linear51,
            constant358,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        gatherelements1_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>) {
        let slice9_out1 = gatherelements1_out1.clone().slice(s![.., .., 2..]);
        let constant359_out1 = self.constant359.val();
        let mul77_out1 = constant359_out1.mul(slice9_out1.clone());
        let slice10_out1 = gatherelements1_out1.slice(s![.., .., 0..2]);
        let add69_out1 = mul77_out1.add(slice10_out1);
        let constant360_out1 = self.constant360.val();
        let mul78_out1 = constant360_out1.mul(slice9_out1);
        let concat6_out1 = burn::tensor::Tensor::cat([add69_out1, mul78_out1].into(), 2);
        let slice11_out1 = concat6_out1.clone().slice(s![.., .., 0..4]);
        let gather1_out1 = {
            let sliced = slice11_out1.clone().slice(s![.., .., 0]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let constant318_out1 = self.constant318.val();
        let mul79_out1 = gather1_out1
            .mul((constant318_out1.clone()).unsqueeze_dims(&[0isize]));
        let gather2_out1 = {
            let sliced = slice11_out1.clone().slice(s![.., .., 1]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let mul80_out1 = gather2_out1
            .mul((constant318_out1.clone()).unsqueeze_dims(&[0isize]));
        let unsqueeze2_out1: Tensor<B, 3> = mul79_out1.unsqueeze_dims::<3>(&[2]);
        let constant319_out1 = self.constant319.val();
        let div12_out1 = unsqueeze2_out1
            .div((constant319_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let unsqueeze3_out1: Tensor<B, 3> = mul80_out1.unsqueeze_dims::<3>(&[2]);
        let div13_out1 = unsqueeze3_out1
            .div((constant319_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let slice12_out1 = div12_out1.clone().slice(s![.., .., 0..; 2]);
        let sin1_out1 = slice12_out1.sin();
        let slice13_out1 = div12_out1.slice(s![.., .., 1..; 2]);
        let cos1_out1 = slice13_out1.cos();
        let unsqueeze4_out1: Tensor<B, 4> = sin1_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze5_out1: Tensor<B, 4> = cos1_out1.unsqueeze_dims::<4>(&[3]);
        let concat7_out1 = burn::tensor::Tensor::cat(
            [unsqueeze4_out1, unsqueeze5_out1].into(),
            3,
        );
        let reshape63_out1 = concat7_out1.reshape([8, 300, -1]);
        let slice14_out1 = div13_out1.clone().slice(s![.., .., 0..; 2]);
        let sin2_out1 = slice14_out1.sin();
        let slice15_out1 = div13_out1.slice(s![.., .., 1..; 2]);
        let cos2_out1 = slice15_out1.cos();
        let unsqueeze6_out1: Tensor<B, 4> = sin2_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze7_out1: Tensor<B, 4> = cos2_out1.unsqueeze_dims::<4>(&[3]);
        let concat8_out1 = burn::tensor::Tensor::cat(
            [unsqueeze6_out1, unsqueeze7_out1].into(),
            3,
        );
        let reshape64_out1 = concat8_out1.reshape([8, 300, -1]);
        let gather3_out1 = {
            let sliced = slice11_out1.clone().slice(s![.., .., 2]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let mul81_out1 = gather3_out1
            .mul((constant318_out1.clone()).unsqueeze_dims(&[0isize]));
        let unsqueeze8_out1: Tensor<B, 3> = mul81_out1.unsqueeze_dims::<3>(&[2]);
        let div14_out1 = unsqueeze8_out1
            .div((constant319_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let slice16_out1 = div14_out1.clone().slice(s![.., .., 0..; 2]);
        let sin3_out1 = slice16_out1.sin();
        let slice17_out1 = div14_out1.slice(s![.., .., 1..; 2]);
        let cos3_out1 = slice17_out1.cos();
        let unsqueeze9_out1: Tensor<B, 4> = sin3_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze10_out1: Tensor<B, 4> = cos3_out1.unsqueeze_dims::<4>(&[3]);
        let concat9_out1 = burn::tensor::Tensor::cat(
            [unsqueeze9_out1, unsqueeze10_out1].into(),
            3,
        );
        let reshape65_out1 = concat9_out1.reshape([8, 300, -1]);
        let gather4_out1 = {
            let sliced = slice11_out1.clone().slice(s![.., .., 3]);
            sliced.squeeze_dim::<2usize>(2)
        };
        let mul82_out1 = gather4_out1.mul((constant318_out1).unsqueeze_dims(&[0isize]));
        let unsqueeze11_out1: Tensor<B, 3> = mul82_out1.unsqueeze_dims::<3>(&[2]);
        let div15_out1 = unsqueeze11_out1
            .div((constant319_out1).unsqueeze_dims(&[0isize, 1isize]));
        let slice18_out1 = div15_out1.clone().slice(s![.., .., 0..; 2]);
        let sin4_out1 = slice18_out1.sin();
        let slice19_out1 = div15_out1.slice(s![.., .., 1..; 2]);
        let cos4_out1 = slice19_out1.cos();
        let unsqueeze12_out1: Tensor<B, 4> = sin4_out1.unsqueeze_dims::<4>(&[3]);
        let unsqueeze13_out1: Tensor<B, 4> = cos4_out1.unsqueeze_dims::<4>(&[3]);
        let concat10_out1 = burn::tensor::Tensor::cat(
            [unsqueeze12_out1, unsqueeze13_out1].into(),
            3,
        );
        let reshape66_out1 = concat10_out1.reshape([8, 300, -1]);
        let concat11_out1 = burn::tensor::Tensor::cat(
            [reshape64_out1, reshape63_out1, reshape65_out1, reshape66_out1].into(),
            2,
        );
        let linear50_out1 = self.linear50.forward(concat11_out1);
        let relu3_out1 = burn::tensor::activation::relu(linear50_out1);
        let linear51_out1 = self.linear51.forward(relu3_out1);
        let constant358_out1 = self.constant358.val();
        (constant358_out1, linear51_out1, slice11_out1, concat6_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule7<B: Backend> {
    linear52: Linear<B>,
    linear53: Linear<B>,
    constant367: burn::module::Param<Tensor<B, 4>>,
    linear54: Linear<B>,
    layernormalization37: LayerNorm<B>,
    linear55: Linear<B>,
    linear56: Linear<B>,
    linear57: Linear<B>,
    constant321: burn::module::Param<Tensor<B, 1>>,
    linear58: Linear<B>,
    layernormalization38: LayerNorm<B>,
    linear59: Linear<B>,
    linear60: Linear<B>,
    layernormalization39: LayerNorm<B>,
    linear61: Linear<B>,
    linear62: Linear<B>,
    linear63: Linear<B>,
    linear64: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule7<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let linear52 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear53 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let constant367: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([8, 8, 300, 32], (device, burn::tensor::DType::F32)),
            device.clone(),
            false,
            [8, 8, 300, 32].into(),
        );
        let linear54 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        let layernormalization37 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear55 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear56 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear57 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let constant321: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
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
        let linear58 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization38 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear59 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear60 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization39 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear61 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear62 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear63 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear64 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        Self {
            linear52,
            linear53,
            constant367,
            linear54,
            layernormalization37,
            linear55,
            linear56,
            linear57,
            constant321,
            linear58,
            layernormalization38,
            linear59,
            linear60,
            layernormalization39,
            linear61,
            linear62,
            linear63,
            linear64,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        constant358_out1: Tensor<B, 3>,
        linear51_out1: Tensor<B, 3>,
        transpose73_out1: Tensor<B, 3>,
        slice11_out1: Tensor<B, 3>,
        constant312_out1: Tensor<B, 1>,
        constant311_out1: Tensor<B, 1>,
    ) -> (Tensor<B, 3>, Tensor<B, 1>, Tensor<B, 6>, Tensor<B, 6>) {
        let add70_out1 = constant358_out1.clone().add(linear51_out1.clone());
        let transpose74_out1 = add70_out1.permute([1, 0, 2]);
        let linear52_out1 = self.linear52.forward(transpose74_out1.clone());
        let linear53_out1 = self.linear53.forward(transpose74_out1);
        let reshape67_out1 = linear52_out1.reshape([300, -1, 32]);
        let transpose75_out1 = reshape67_out1.permute([1, 0, 2]);
        let reshape68_out1 = linear53_out1.reshape([300, -1, 32]);
        let transpose76_out1 = reshape68_out1.permute([1, 0, 2]);
        let reshape69_out1 = transpose75_out1.reshape([-1, 8, 300, 32]);
        let reshape70_out1 = transpose76_out1.reshape([-1, 8, 300, 32]);
        let constant367_out1 = self.constant367.val();
        let (matmul77_out1,) = {
            let q = reshape69_out1;
            let k = reshape70_out1;
            let v = constant367_out1;
            let matmul77_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1767767071723938f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul77_out1,)
        };
        let transpose78_out1 = matmul77_out1.permute([2, 0, 1, 3]);
        let reshape71_out1 = transpose78_out1.reshape([-1, 256]);
        let linear54_out1 = self.linear54.forward(reshape71_out1);
        let reshape72_out1 = linear54_out1.reshape([300, -1, 256]);
        let transpose79_out1 = reshape72_out1.permute([1, 0, 2]);
        let add71_out1 = constant358_out1.add(transpose79_out1);
        let layernormalization37_out1 = {
            let dtype = add71_out1.dtype();
            self.layernormalization37
                .forward(add71_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add72_out1 = layernormalization37_out1.clone().add(linear51_out1.clone());
        let linear55_out1 = self.linear55.forward(transpose73_out1);
        let linear56_out1 = self.linear56.forward(add72_out1.clone());
        let reshape73_out1 = linear56_out1.reshape([-1, 300, 16, 1, 2, 2]);
        let linear57_out1 = self.linear57.forward(add72_out1);
        let reshape74_out1 = linear57_out1.reshape([-1, 300, 16, 2]);
        let unsqueeze14_out1: Tensor<B, 6> = slice11_out1
            .unsqueeze_dims::<6>(&[2, 3, 4]);
        let slice20_out1 = unsqueeze14_out1.clone().slice(s![.., .., .., .., .., 0..2]);
        let constant321_out1 = self.constant321.val();
        let div16_out1 = reshape73_out1
            .div(
                (constant321_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let slice21_out1 = unsqueeze14_out1.slice(s![.., .., .., .., .., 2..]);
        let mul85_out1 = div16_out1.mul(slice21_out1.clone());
        let mul86_out1 = mul85_out1
            .mul(
                (constant312_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add73_out1 = slice20_out1.clone().add(mul86_out1);
        let softmax13_out1 = burn::tensor::activation::softmax(reshape74_out1, 3);
        let transpose80_out1 = linear55_out1.permute([0, 2, 1]);
        let reshape75_out1 = transpose80_out1.reshape([8, 16, 16, 1600]);
        let slice22_out1 = reshape75_out1.slice(s![.., .., .., 0..1600]);
        let mul87_out1 = add73_out1
            .mul(
                (constant321_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let sub1_out1 = mul87_out1
            .sub(
                (constant311_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let reshape76_out1 = slice22_out1.reshape([128, 16, 40, 40]);
        let gather5_out1 = {
            let sliced = sub1_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose81_out1 = gather5_out1.permute([0, 2, 1, 3, 4]);
        let reshape77_out1 = transpose81_out1.reshape([-1, 300, 2, 2]);
        let gridsample1_out1 = reshape76_out1
            .grid_sample_2d(
                reshape77_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose82_out1 = softmax13_out1.permute([0, 2, 1, 3]);
        let reshape78_out1 = transpose82_out1.reshape([-1, 1, 300, 2]);
        let unsqueeze15_out1: Tensor<B, 5> = gridsample1_out1.unsqueeze_dims::<5>(&[-2]);
        let reshape79_out1 = unsqueeze15_out1.reshape([128, 16, 300, -1]);
        let mul88_out1 = reshape79_out1.mul(reshape78_out1);
        let reducesum1_out1 = {
            mul88_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let reshape80_out1 = reducesum1_out1.reshape([8, 256, 300]);
        let transpose83_out1 = reshape80_out1.permute([0, 2, 1]);
        let linear58_out1 = self.linear58.forward(transpose83_out1);
        let add74_out1 = layernormalization37_out1.add(linear58_out1);
        let layernormalization38_out1 = {
            let dtype = add74_out1.dtype();
            self.layernormalization38
                .forward(add74_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear59_out1 = self.linear59.forward(layernormalization38_out1.clone());
        let relu4_out1 = burn::tensor::activation::relu(linear59_out1);
        let linear60_out1 = self.linear60.forward(relu4_out1);
        let add75_out1 = layernormalization38_out1.add(linear60_out1);
        let layernormalization39_out1 = {
            let dtype = add75_out1.dtype();
            self.layernormalization39
                .forward(add75_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add76_out1 = layernormalization39_out1.clone().add(linear51_out1);
        let transpose84_out1 = add76_out1.permute([1, 0, 2]);
        let transpose85_out1 = layernormalization39_out1.clone().permute([1, 0, 2]);
        let linear61_out1 = self.linear61.forward(transpose84_out1.clone());
        let linear62_out1 = self.linear62.forward(transpose84_out1);
        let linear63_out1 = self.linear63.forward(transpose85_out1);
        let reshape81_out1 = linear61_out1.reshape([300, -1, 32]);
        let transpose86_out1 = reshape81_out1.permute([1, 0, 2]);
        let reshape82_out1 = linear62_out1.reshape([300, -1, 32]);
        let transpose87_out1 = reshape82_out1.permute([1, 0, 2]);
        let reshape83_out1 = linear63_out1.reshape([300, -1, 32]);
        let transpose88_out1 = reshape83_out1.permute([1, 0, 2]);
        let reshape84_out1 = transpose86_out1.reshape([-1, 8, 300, 32]);
        let reshape85_out1 = transpose87_out1.reshape([-1, 8, 300, 32]);
        let reshape86_out1 = transpose88_out1.reshape([-1, 8, 300, 32]);
        let (matmul88_out1,) = {
            let q = reshape84_out1;
            let k = reshape85_out1;
            let v = reshape86_out1;
            let matmul88_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1767767071723938f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul88_out1,)
        };
        let transpose90_out1 = matmul88_out1.permute([2, 0, 1, 3]);
        let reshape87_out1 = transpose90_out1.reshape([-1, 256]);
        let linear64_out1 = self.linear64.forward(reshape87_out1);
        let reshape88_out1 = linear64_out1.reshape([300, -1, 256]);
        let transpose91_out1 = reshape88_out1.permute([1, 0, 2]);
        let add77_out1 = layernormalization39_out1.add(transpose91_out1);
        (add77_out1, constant321_out1, slice21_out1, slice20_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule8<B: Backend> {
    layernormalization40: LayerNorm<B>,
    linear65: Linear<B>,
    linear66: Linear<B>,
    linear67: Linear<B>,
    linear68: Linear<B>,
    layernormalization41: LayerNorm<B>,
    linear69: Linear<B>,
    linear70: Linear<B>,
    layernormalization42: LayerNorm<B>,
    linear71: Linear<B>,
    linear72: Linear<B>,
    linear73: Linear<B>,
    linear74: Linear<B>,
    layernormalization43: LayerNorm<B>,
    linear75: Linear<B>,
    linear76: Linear<B>,
    linear77: Linear<B>,
    linear78: Linear<B>,
    layernormalization44: LayerNorm<B>,
    linear79: Linear<B>,
    linear80: Linear<B>,
    layernormalization45: LayerNorm<B>,
    layernormalization46: LayerNorm<B>,
    linear81: Linear<B>,
    linear82: Linear<B>,
    linear83: Linear<B>,
    linear84: Linear<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule8<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let layernormalization40 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear65 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear66 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear67 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let linear68 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization41 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear69 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear70 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization42 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear71 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear72 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear73 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear74 = LinearConfig::new(256, 256)
            .with_bias(true)
            .with_layout(LinearLayout::Col)
            .init(device);
        let layernormalization43 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear75 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear76 = LinearConfig::new(256, 64).with_bias(true).init(device);
        let linear77 = LinearConfig::new(256, 32).with_bias(true).init(device);
        let linear78 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let layernormalization44 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear79 = LinearConfig::new(256, 2048).with_bias(true).init(device);
        let linear80 = LinearConfig::new(2048, 256).with_bias(true).init(device);
        let layernormalization45 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let layernormalization46 = LayerNormConfig::new(256)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear81 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear82 = LinearConfig::new(256, 256).with_bias(true).init(device);
        let linear83 = LinearConfig::new(256, 4).with_bias(true).init(device);
        let linear84 = LinearConfig::new(256, 18).with_bias(true).init(device);
        Self {
            layernormalization40,
            linear65,
            linear66,
            linear67,
            linear68,
            layernormalization41,
            linear69,
            linear70,
            layernormalization42,
            linear71,
            linear72,
            linear73,
            linear74,
            layernormalization43,
            linear75,
            linear76,
            linear77,
            linear78,
            layernormalization44,
            linear79,
            linear80,
            layernormalization45,
            layernormalization46,
            linear81,
            linear82,
            linear83,
            linear84,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add77_out1: Tensor<B, 3>,
        linear51_out1: Tensor<B, 3>,
        transpose73_out1: Tensor<B, 3>,
        constant321_out1: Tensor<B, 1>,
        slice21_out1: Tensor<B, 6>,
        constant312_out1: Tensor<B, 1>,
        slice20_out1: Tensor<B, 6>,
        constant311_out1: Tensor<B, 1>,
        concat6_out1: Tensor<B, 3>,
    ) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let layernormalization40_out1 = {
            let dtype = add77_out1.dtype();
            self.layernormalization40
                .forward(add77_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add78_out1 = layernormalization40_out1.clone().add(linear51_out1.clone());
        let linear65_out1 = self.linear65.forward(transpose73_out1.clone());
        let linear66_out1 = self.linear66.forward(add78_out1.clone());
        let reshape89_out1 = linear66_out1.reshape([-1, 300, 16, 1, 2, 2]);
        let linear67_out1 = self.linear67.forward(add78_out1);
        let reshape90_out1 = linear67_out1.reshape([-1, 300, 16, 2]);
        let div17_out1 = reshape89_out1
            .div(
                (constant321_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul91_out1 = div17_out1.mul(slice21_out1.clone());
        let mul92_out1 = mul91_out1
            .mul(
                (constant312_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add79_out1 = slice20_out1.clone().add(mul92_out1);
        let softmax15_out1 = burn::tensor::activation::softmax(reshape90_out1, 3);
        let transpose92_out1 = linear65_out1.permute([0, 2, 1]);
        let reshape91_out1 = transpose92_out1.reshape([8, 16, 16, 1600]);
        let slice23_out1 = reshape91_out1.slice(s![.., .., .., 0..1600]);
        let mul93_out1 = add79_out1
            .mul(
                (constant321_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let sub2_out1 = mul93_out1
            .sub(
                (constant311_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let reshape92_out1 = slice23_out1.reshape([128, 16, 40, 40]);
        let gather6_out1 = {
            let sliced = sub2_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose93_out1 = gather6_out1.permute([0, 2, 1, 3, 4]);
        let reshape93_out1 = transpose93_out1.reshape([-1, 300, 2, 2]);
        let gridsample2_out1 = reshape92_out1
            .grid_sample_2d(
                reshape93_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose94_out1 = softmax15_out1.permute([0, 2, 1, 3]);
        let reshape94_out1 = transpose94_out1.reshape([-1, 1, 300, 2]);
        let unsqueeze16_out1: Tensor<B, 5> = gridsample2_out1.unsqueeze_dims::<5>(&[-2]);
        let reshape95_out1 = unsqueeze16_out1.reshape([128, 16, 300, -1]);
        let mul94_out1 = reshape95_out1.mul(reshape94_out1);
        let reducesum2_out1 = {
            mul94_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let reshape96_out1 = reducesum2_out1.reshape([8, 256, 300]);
        let transpose95_out1 = reshape96_out1.permute([0, 2, 1]);
        let linear68_out1 = self.linear68.forward(transpose95_out1);
        let add80_out1 = layernormalization40_out1.add(linear68_out1);
        let layernormalization41_out1 = {
            let dtype = add80_out1.dtype();
            self.layernormalization41
                .forward(add80_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear69_out1 = self.linear69.forward(layernormalization41_out1.clone());
        let relu5_out1 = burn::tensor::activation::relu(linear69_out1);
        let linear70_out1 = self.linear70.forward(relu5_out1);
        let add81_out1 = layernormalization41_out1.add(linear70_out1);
        let layernormalization42_out1 = {
            let dtype = add81_out1.dtype();
            self.layernormalization42
                .forward(add81_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add82_out1 = layernormalization42_out1.clone().add(linear51_out1.clone());
        let transpose96_out1 = add82_out1.permute([1, 0, 2]);
        let transpose97_out1 = layernormalization42_out1.clone().permute([1, 0, 2]);
        let linear71_out1 = self.linear71.forward(transpose96_out1.clone());
        let linear72_out1 = self.linear72.forward(transpose96_out1);
        let linear73_out1 = self.linear73.forward(transpose97_out1);
        let reshape97_out1 = linear71_out1.reshape([300, -1, 32]);
        let transpose98_out1 = reshape97_out1.permute([1, 0, 2]);
        let reshape98_out1 = linear72_out1.reshape([300, -1, 32]);
        let transpose99_out1 = reshape98_out1.permute([1, 0, 2]);
        let reshape99_out1 = linear73_out1.reshape([300, -1, 32]);
        let transpose100_out1 = reshape99_out1.permute([1, 0, 2]);
        let reshape100_out1 = transpose98_out1.reshape([-1, 8, 300, 32]);
        let reshape101_out1 = transpose99_out1.reshape([-1, 8, 300, 32]);
        let reshape102_out1 = transpose100_out1.reshape([-1, 8, 300, 32]);
        let (matmul99_out1,) = {
            let q = reshape100_out1;
            let k = reshape101_out1;
            let v = reshape102_out1;
            let matmul99_out1 = burn::tensor::module::attention(
                q,
                k,
                v,
                None,
                None,
                burn::tensor::ops::AttentionModuleOptions {
                    scale: Some(0.1767767071723938f64),
                    softcap: None,
                    is_causal: false,
                },
            );
            (matmul99_out1,)
        };
        let transpose102_out1 = matmul99_out1.permute([2, 0, 1, 3]);
        let reshape103_out1 = transpose102_out1.reshape([-1, 256]);
        let linear74_out1 = self.linear74.forward(reshape103_out1);
        let reshape104_out1 = linear74_out1.reshape([300, -1, 256]);
        let transpose103_out1 = reshape104_out1.permute([1, 0, 2]);
        let add83_out1 = layernormalization42_out1.add(transpose103_out1);
        let layernormalization43_out1 = {
            let dtype = add83_out1.dtype();
            self.layernormalization43
                .forward(add83_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add84_out1 = layernormalization43_out1.clone().add(linear51_out1);
        let linear75_out1 = self.linear75.forward(transpose73_out1);
        let linear76_out1 = self.linear76.forward(add84_out1.clone());
        let reshape105_out1 = linear76_out1.reshape([-1, 300, 16, 1, 2, 2]);
        let linear77_out1 = self.linear77.forward(add84_out1);
        let reshape106_out1 = linear77_out1.reshape([-1, 300, 16, 2]);
        let div18_out1 = reshape105_out1
            .div(
                (constant321_out1.clone())
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let mul97_out1 = div18_out1.mul(slice21_out1);
        let mul98_out1 = mul97_out1
            .mul(
                (constant312_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let add85_out1 = slice20_out1.add(mul98_out1);
        let softmax17_out1 = burn::tensor::activation::softmax(reshape106_out1, 3);
        let transpose104_out1 = linear75_out1.permute([0, 2, 1]);
        let reshape107_out1 = transpose104_out1.reshape([8, 16, 16, 1600]);
        let slice24_out1 = reshape107_out1.slice(s![.., .., .., 0..1600]);
        let mul99_out1 = add85_out1
            .mul(
                (constant321_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let sub3_out1 = mul99_out1
            .sub(
                (constant311_out1)
                    .unsqueeze_dims(&[0isize, 1isize, 2isize, 3isize, 4isize]),
            );
        let reshape108_out1 = slice24_out1.reshape([128, 16, 40, 40]);
        let gather7_out1 = {
            let sliced = sub3_out1.slice(s![.., .., .., 0, .., ..]);
            sliced.squeeze_dim::<5usize>(3)
        };
        let transpose105_out1 = gather7_out1.permute([0, 2, 1, 3, 4]);
        let reshape109_out1 = transpose105_out1.reshape([-1, 300, 2, 2]);
        let gridsample3_out1 = reshape108_out1
            .grid_sample_2d(
                reshape109_out1,
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(false),
            );
        let transpose106_out1 = softmax17_out1.permute([0, 2, 1, 3]);
        let reshape110_out1 = transpose106_out1.reshape([-1, 1, 300, 2]);
        let unsqueeze17_out1: Tensor<B, 5> = gridsample3_out1.unsqueeze_dims::<5>(&[-2]);
        let reshape111_out1 = unsqueeze17_out1.reshape([128, 16, 300, -1]);
        let mul100_out1 = reshape111_out1.mul(reshape110_out1);
        let reducesum3_out1 = {
            mul100_out1.sum_dim(3usize).squeeze_dims::<3usize>(&[3])
        };
        let reshape112_out1 = reducesum3_out1.reshape([8, 256, 300]);
        let transpose107_out1 = reshape112_out1.permute([0, 2, 1]);
        let linear78_out1 = self.linear78.forward(transpose107_out1);
        let add86_out1 = layernormalization43_out1.add(linear78_out1);
        let layernormalization44_out1 = {
            let dtype = add86_out1.dtype();
            self.layernormalization44
                .forward(add86_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear79_out1 = self.linear79.forward(layernormalization44_out1.clone());
        let relu6_out1 = burn::tensor::activation::relu(linear79_out1);
        let linear80_out1 = self.linear80.forward(relu6_out1);
        let add87_out1 = layernormalization44_out1.add(linear80_out1);
        let layernormalization45_out1 = {
            let dtype = add87_out1.dtype();
            self.layernormalization45
                .forward(add87_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let layernormalization46_out1 = {
            let dtype = layernormalization45_out1.dtype();
            self.layernormalization46
                .forward(layernormalization45_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let linear81_out1 = self.linear81.forward(layernormalization46_out1.clone());
        let relu7_out1 = burn::tensor::activation::relu(linear81_out1);
        let linear82_out1 = self.linear82.forward(relu7_out1);
        let relu8_out1 = burn::tensor::activation::relu(linear82_out1);
        let linear83_out1 = self.linear83.forward(relu8_out1);
        let slice25_out1 = linear83_out1.clone().slice(s![.., .., 0..2]);
        let slice26_out1 = concat6_out1.clone().slice(s![.., .., 2..]);
        let mul101_out1 = slice25_out1.mul(slice26_out1.clone());
        let slice27_out1 = concat6_out1.slice(s![.., .., 0..2]);
        let add88_out1 = mul101_out1.add(slice27_out1);
        let slice28_out1 = linear83_out1.slice(s![.., .., 2..]);
        let exp2_out1 = slice28_out1.exp();
        let mul102_out1 = exp2_out1.mul(slice26_out1);
        let concat12_out1 = burn::tensor::Tensor::cat(
            [add88_out1, mul102_out1].into(),
            2,
        );
        let linear84_out1 = self.linear84.forward(layernormalization46_out1);
        (concat12_out1, linear84_out1)
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
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}


extern crate std;

impl<B: Backend> Default for Model<B> {
    fn default() -> Self {
        Self::from_file(
            "/tmp/onnx2gen-2122917-rfdetr-base-b8/rfdetr-base-b8.bpk",
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
        Self {
            submodule1,
            submodule2,
            submodule3,
            submodule4,
            submodule5,
            submodule6,
            submodule7,
            submodule8,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }

    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, input: Tensor<B, 4>) -> (Tensor<B, 3>, Tensor<B, 3>) {
        let (add11_out1, constant310_out1, constant311_out1, constant312_out1) = self
            .submodule1
            .forward(input);
        let (add23_out1, add13_out1) = self
            .submodule2
            .forward(
                add11_out1,
                constant310_out1.clone(),
                constant311_out1.clone(),
                constant312_out1.clone(),
            );
        let (transpose26_out1, add31_out1) = self
            .submodule3
            .forward(
                add23_out1,
                constant310_out1.clone(),
                constant311_out1.clone(),
                constant312_out1.clone(),
            );
        let concat3_out1 = self
            .submodule4
            .forward(
                transpose26_out1,
                add31_out1,
                constant310_out1,
                constant311_out1.clone(),
                constant312_out1.clone(),
                add13_out1,
            );
        let (gatherelements1_out1, transpose73_out1) = self
            .submodule5
            .forward(concat3_out1);
        let (constant358_out1, linear51_out1, slice11_out1, concat6_out1) = self
            .submodule6
            .forward(gatherelements1_out1);
        let (add77_out1, constant321_out1, slice21_out1, slice20_out1) = self
            .submodule7
            .forward(
                constant358_out1,
                linear51_out1.clone(),
                transpose73_out1.clone(),
                slice11_out1,
                constant312_out1.clone(),
                constant311_out1.clone(),
            );
        let (concat12_out1, linear84_out1) = self
            .submodule8
            .forward(
                add77_out1,
                linear51_out1,
                transpose73_out1,
                constant321_out1,
                slice21_out1,
                constant312_out1,
                slice20_out1,
                constant311_out1,
                concat6_out1,
            );
        (concat12_out1, linear84_out1)
    }
}
