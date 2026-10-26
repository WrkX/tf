//! speech-engine und diarizer laden ihre `.mlmodelc`-bundles über dieses modul. es hängt nur an den objective-c-bindings, nie an appkit.

use std::path::{Path, PathBuf};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model file not found: {0}")]
    MissingModel(PathBuf),
    #[error("CoreML: {0}")]
    CoreMl(String),
}

pub struct Model {
    inner: Retained<MLModel>,
    name: String,
}

/// eigener core ml multi-array. shape und daten sind beim anlegen fix.
pub struct Tensor(Retained<MLMultiArray>);

pub struct Outputs<'a> {
    provider: Retained<ProtocolObject<dyn MLFeatureProvider>>,
    model: &'a str,
}

// SAFETY: mlmodel und mlmultiarray haben keine thread-affinität. beide wrapper gehören genau einem besitzer und werden nie geteilt.
unsafe impl Send for Model {}
unsafe impl Send for Tensor {}

#[derive(Clone, Copy, Debug)]
pub enum Compute {
    Cpu,
    NeuralEngine,
}

impl Model {
    pub fn load(path: &Path, compute: Compute) -> Result<Self, Error> {
        if !path.exists() {
            return Err(Error::MissingModel(path.into()));
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let url = NSURL::from_file_path(path).ok_or_else(|| Error::MissingModel(path.into()))?;
        let units = match compute {
            Compute::Cpu => MLComputeUnits::CPUOnly,
            Compute::NeuralEngine => MLComputeUnits::CPUAndNeuralEngine,
        };
        let inner = unsafe {
            let config = MLModelConfiguration::new();
            config.setComputeUnits(units);
            MLModel::modelWithContentsOfURL_configuration_error(&url, &config)
        }
        .map_err(|e| coreml_error(&name, e))?;
        Ok(Self { inner, name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn predict(&self, inputs: &[(&str, &Tensor)]) -> Result<Outputs<'_>, Error> {
        let keys: Vec<Retained<NSString>> = inputs.iter().map(|(k, _)| NSString::from_str(k)).collect();
        let values: Vec<Retained<AnyObject>> = inputs
            .iter()
            .map(|(_, t)| unsafe {
                Retained::into_super(Retained::into_super(MLFeatureValue::featureValueWithMultiArray(&t.0)))
            })
            .collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
        let dict = NSDictionary::from_slices(&key_refs, &value_refs);

        let provider = unsafe {
            MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dict)
        }
        .map_err(|e| coreml_error(&self.name, e))?;
        let provider = ProtocolObject::from_retained(provider);
        let output = unsafe { self.inner.predictionFromFeatures_error(&provider) }
            .map_err(|e| coreml_error(&self.name, e))?;
        Ok(Outputs { provider: output, model: &self.name })
    }
}

impl Outputs<'_> {
    pub fn get(&self, name: &str) -> Result<Tensor, Error> {
        unsafe { self.provider.featureValueForName(&NSString::from_str(name)) }
            .and_then(|v| unsafe { v.multiArrayValue() })
            .map(Tensor)
            .ok_or_else(|| Error::CoreMl(format!("{}: missing output `{name}`", self.model)))
    }
}

impl Tensor {
    pub fn zeros_f32(shape: &[usize]) -> Result<Self, Error> {
        Self::new(shape, MLMultiArrayDataType::Float32)
    }

    pub fn i32(shape: &[usize], value: i32) -> Result<Self, Error> {
        let t = Self::new(shape, MLMultiArrayDataType::Int32)?;
        t.write_i32(&[value]);
        Ok(t)
    }

    fn new(shape: &[usize], dtype: MLMultiArrayDataType) -> Result<Self, Error> {
        let dims: Vec<Retained<NSNumber>> = shape.iter().map(|&d| NSNumber::new_usize(d)).collect();
        let shape = NSArray::from_retained_slice(&dims);
        let array =
            unsafe { MLMultiArray::initWithShape_dataType_error(MLMultiArray::alloc(), &shape, dtype) }
                .map_err(|e| coreml_error("MLMultiArray", e))?;
        let t = Self(array);
        t.fill_zero();
        Ok(t)
    }

    pub fn len(&self) -> usize {
        unsafe { self.0.count() as usize }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dtype(&self) -> MLMultiArrayDataType {
        unsafe { self.0.dataType() }
    }

    pub fn shape(&self) -> Vec<usize> {
        unsafe { self.0.shape() }.iter().map(|n| n.as_usize()).collect()
    }

    fn strides(&self) -> Vec<usize> {
        unsafe { self.0.strides() }.iter().map(|n| n.as_usize()).collect()
    }

    #[allow(deprecated)]
    fn ptr(&self) -> *mut u8 {
        unsafe { self.0.dataPointer().as_ptr().cast() }
    }

    fn fill_zero(&self) {
        let bytes = self.len() * element_size(self.dtype());
        unsafe { std::ptr::write_bytes(self.ptr(), 0, bytes) };
    }

    pub fn write_f32(&self, data: &[f32]) {
        self.write_f32_at(0, data);
    }

    /// schreibt in den linearen puffer ab `offset`, geklemmt auf die tensor-größe.
    pub fn write_f32_at(&self, offset: usize, data: &[f32]) {
        assert_eq!(self.dtype(), MLMultiArrayDataType::Float32);
        let available = self.len().saturating_sub(offset);
        let n = data.len().min(available);
        if n == 0 {
            return;
        }
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr().cast::<f32>().add(offset), n) };
    }

    pub fn write_i32(&self, data: &[i32]) {
        assert_eq!(self.dtype(), MLMultiArrayDataType::Int32);
        let n = data.len().min(self.len());
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr().cast::<i32>(), n) };
    }

    pub fn copy_from(&self, other: &Tensor) {
        self.write_f32(&other.read_f32());
    }

    /// liest einen float-tensor in logischer row-major-reihenfolge, strides werden beachtet.
    pub fn read_f32(&self) -> Vec<f32> {
        assert_eq!(self.dtype(), MLMultiArrayDataType::Float32);
        let (shape, strides) = (self.shape(), self.strides());
        let base = self.ptr().cast::<f32>();
        (0..self.len())
            .map(|mut i| {
                let mut offset = 0;
                for (dim, stride) in shape.iter().zip(&strides).rev() {
                    offset += (i % dim) * stride;
                    i /= dim;
                }
                unsafe { *base.add(offset) }
            })
            .collect()
    }

    pub fn first_i32(&self) -> i32 {
        match self.dtype() {
            MLMultiArrayDataType::Int32 => unsafe { *self.ptr().cast::<i32>() },
            other => panic!("expected Int32 tensor, got {other:?}"),
        }
    }

    /// transponiert einen `[1, C, T]`-float-tensor in `T` zusammenhängende frames zu `C` werten.
    pub fn frame_major(&self) -> Vec<f32> {
        assert_eq!(self.dtype(), MLMultiArrayDataType::Float32);
        let (shape, strides) = (self.shape(), self.strides());
        let (channels, frames) = (shape[1], shape[2]);
        let base = self.ptr().cast::<f32>();
        let mut out = Vec::with_capacity(channels * frames);
        for t in 0..frames {
            for c in 0..channels {
                out.push(unsafe { *base.add(c * strides[1] + t * strides[2]) });
            }
        }
        out
    }
}

fn element_size(dtype: MLMultiArrayDataType) -> usize {
    match dtype {
        MLMultiArrayDataType::Float16 => 2,
        MLMultiArrayDataType::Double => 8,
        _ => 4,
    }
}

fn coreml_error(context: &str, error: Retained<NSError>) -> Error {
    Error::CoreMl(format!("{context}: {}", error.localizedDescription()))
}
