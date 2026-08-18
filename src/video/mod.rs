//! Deterministic, renderer-independent animation primitives.
//!
//! Clips transform time, properties produce values, the evaluator resolves an
//! immutable snapshot, and renderers only display that snapshot. Browser clocks
//! and renderer state are deliberately outside this module.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

pub type Seconds = f64;
pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Bool(bool),
    Text(String),
    Vec2(Vec2),
    Color(Color),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Number,
    Bool,
    Text,
    Vec2,
    Color,
}

pub trait AnimatableValue: Clone + Send + Sync + 'static {
    const TYPE: ValueType;
    fn into_value(self) -> Value;
    fn from_value(value: &Value) -> Option<Self>;
    fn interpolate(left: &Self, right: &Self, amount: f64) -> Option<Self>;
}

impl AnimatableValue for f64 {
    const TYPE: ValueType = ValueType::Number;
    fn into_value(self) -> Value {
        Value::Number(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Number(v) = value {
            Some(*v)
        } else {
            None
        }
    }
    fn interpolate(left: &Self, right: &Self, amount: f64) -> Option<Self> {
        Some(left + (right - left) * amount)
    }
}

impl AnimatableValue for bool {
    const TYPE: ValueType = ValueType::Bool;
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Bool(v) = value {
            Some(*v)
        } else {
            None
        }
    }
    fn interpolate(_: &Self, _: &Self, _: f64) -> Option<Self> {
        None
    }
}

impl AnimatableValue for String {
    const TYPE: ValueType = ValueType::Text;
    fn into_value(self) -> Value {
        Value::Text(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Text(v) = value {
            Some(v.clone())
        } else {
            None
        }
    }
    fn interpolate(_: &Self, _: &Self, _: f64) -> Option<Self> {
        None
    }
}

impl AnimatableValue for Vec2 {
    const TYPE: ValueType = ValueType::Vec2;
    fn into_value(self) -> Value {
        Value::Vec2(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Vec2(v) = value {
            Some(*v)
        } else {
            None
        }
    }
    fn interpolate(left: &Self, right: &Self, amount: f64) -> Option<Self> {
        Some(Self {
            x: left.x + (right.x - left.x) * amount,
            y: left.y + (right.y - left.y) * amount,
        })
    }
}

impl AnimatableValue for Color {
    const TYPE: ValueType = ValueType::Color;
    fn into_value(self) -> Value {
        Value::Color(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Color(v) = value {
            Some(*v)
        } else {
            None
        }
    }
    fn interpolate(left: &Self, right: &Self, amount: f64) -> Option<Self> {
        Some(Self {
            r: left.r + (right.r - left.r) * amount,
            g: left.g + (right.g - left.g) * amount,
            b: left.b + (right.b - left.b) * amount,
            a: left.a + (right.a - left.a) * amount,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Interpolation {
    Hold,
    Linear,
    /// Cubic Bezier timing curve. Control points are `(x1, y1, x2, y2)`.
    CubicBezier(f64, f64, f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe<T> {
    pub time: Seconds,
    pub value: T,
    /// Interpolation from this key to the next key.
    pub interpolation: Interpolation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extrapolation {
    Default,
    Hold,
    Linear,
    Loop,
    PingPong,
}

#[derive(Clone, Debug)]
pub struct Curve<T> {
    pub keys: Vec<Keyframe<T>>,
    pub pre_extrapolation: Extrapolation,
    pub post_extrapolation: Extrapolation,
}

impl<T: AnimatableValue> Curve<T> {
    pub fn new(keys: Vec<Keyframe<T>>) -> Result<Self, AnimationError> {
        for pair in keys.windows(2) {
            if !pair[0].time.is_finite() || pair[0].time >= pair[1].time {
                return Err(AnimationError::InvalidCurve(
                    "key times must be finite and strictly increasing".into(),
                ));
            }
        }
        if keys.last().is_some_and(|key| !key.time.is_finite()) {
            return Err(AnimationError::InvalidCurve(
                "key times must be finite".into(),
            ));
        }
        Ok(Self {
            keys,
            pre_extrapolation: Extrapolation::Default,
            post_extrapolation: Extrapolation::Default,
        })
    }

    pub fn sample(&self, time: Seconds, default: &T) -> Result<T, AnimationError> {
        match self.keys.as_slice() {
            [] => return Ok(default.clone()),
            [only] => return Ok(only.value.clone()),
            _ => {}
        }
        let first = &self.keys[0];
        let last = self.keys.last().expect("non-empty curve");
        if time < first.time && self.pre_extrapolation == Extrapolation::Linear {
            return Self::sample_segment(&self.keys[0], &self.keys[1], time);
        }
        if time > last.time && self.post_extrapolation == Extrapolation::Linear {
            let length = self.keys.len();
            return Self::sample_segment(&self.keys[length - 2], &self.keys[length - 1], time);
        }
        let mapped = if time < first.time {
            self.map_extrapolated(time, self.pre_extrapolation)?
        } else if time > last.time {
            self.map_extrapolated(time, self.post_extrapolation)?
        } else {
            time
        };
        if mapped <= first.time {
            return Ok(first.value.clone());
        }
        if mapped >= last.time {
            return Ok(last.value.clone());
        }
        let right = self.keys.partition_point(|key| key.time <= mapped);
        Self::sample_segment(&self.keys[right - 1], &self.keys[right], mapped)
    }

    fn sample_segment(
        left: &Keyframe<T>,
        right: &Keyframe<T>,
        time: Seconds,
    ) -> Result<T, AnimationError> {
        let progress = (time - left.time) / (right.time - left.time);
        match left.interpolation {
            Interpolation::Hold => Ok(left.value.clone()),
            Interpolation::Linear => T::interpolate(&left.value, &right.value, progress)
                .ok_or(AnimationError::UnsupportedInterpolation(T::TYPE)),
            Interpolation::CubicBezier(x1, y1, x2, y2) => {
                let eased = cubic_bezier(progress.clamp(0.0, 1.0), x1, y1, x2, y2);
                T::interpolate(&left.value, &right.value, eased)
                    .ok_or(AnimationError::UnsupportedInterpolation(T::TYPE))
            }
        }
    }

    fn map_extrapolated(
        &self,
        time: Seconds,
        mode: Extrapolation,
    ) -> Result<Seconds, AnimationError> {
        let first = self.keys.first().expect("non-empty curve").time;
        let last = self.keys.last().expect("non-empty curve").time;
        let duration = last - first;
        Ok(match mode {
            Extrapolation::Default | Extrapolation::Hold => time.clamp(first, last),
            Extrapolation::Linear => time,
            Extrapolation::Loop => first + positive_modulo(time - first, duration),
            Extrapolation::PingPong => {
                let cycle = positive_modulo(time - first, duration * 2.0);
                first
                    + if cycle <= duration {
                        cycle
                    } else {
                        duration * 2.0 - cycle
                    }
            }
        })
    }
}

fn cubic_bezier(x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let sample = |t: f64, a: f64, b: f64| {
        let inv = 1.0 - t;
        3.0 * inv * inv * t * a + 3.0 * inv * t * t * b + t * t * t
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..20 {
        let middle = (low + high) * 0.5;
        if sample(middle, x1, x2) < x {
            low = middle;
        } else {
            high = middle;
        }
    }
    sample((low + high) * 0.5, y1, y2)
}

#[derive(Clone, Debug)]
pub struct EvaluationContext<'a> {
    values: &'a BTreeMap<Id, Value>,
}

impl<'a> EvaluationContext<'a> {
    pub fn get<T: AnimatableValue>(&self, id: &str) -> Result<T, AnimationError> {
        let value = self
            .values
            .get(id)
            .ok_or_else(|| AnimationError::MissingDependency(id.into()))?;
        T::from_value(value).ok_or_else(|| AnimationError::TypeMismatch {
            id: id.into(),
            expected: T::TYPE,
            actual: value_type(value),
        })
    }
}

type Compute<T> =
    Arc<dyn Fn(&EvaluationContext<'_>, Seconds) -> Result<T, AnimationError> + Send + Sync>;

#[derive(Clone)]
pub enum PropertySource<T> {
    Constant(T),
    Curve(Curve<T>),
    Reference(Id),
    Computed {
        dependencies: Vec<Id>,
        evaluate: Compute<T>,
    },
}

#[derive(Clone)]
pub struct Property<T> {
    pub id: Id,
    pub default_value: T,
    pub source: PropertySource<T>,
}

impl<T: AnimatableValue> Property<T> {
    pub fn erase(self) -> ErasedProperty {
        let id = self.id;
        let value_type = T::TYPE;
        let default = self.default_value.clone();
        let (dependencies, evaluate): (Vec<Id>, ErasedCompute) =
            match self.source {
                PropertySource::Constant(value) => {
                    (vec![], Arc::new(move |_, _| Ok(value.clone().into_value())))
                }
                PropertySource::Curve(curve) => (
                    vec![],
                    Arc::new(move |_, time| Ok(curve.sample(time, &default)?.into_value())),
                ),
                PropertySource::Reference(target) => {
                    let dependency = target.clone();
                    (
                        vec![target],
                        Arc::new(move |context, _| {
                            context.values.get(&dependency).cloned().ok_or_else(|| {
                                AnimationError::MissingDependency(dependency.clone())
                            })
                        }),
                    )
                }
                PropertySource::Computed {
                    dependencies,
                    evaluate,
                } => (
                    dependencies,
                    Arc::new(move |context, time| Ok(evaluate(context, time)?.into_value())),
                ),
            };
        ErasedProperty {
            id,
            value_type,
            dependencies,
            evaluate,
        }
    }
}

type ErasedCompute =
    Arc<dyn Fn(&EvaluationContext<'_>, Seconds) -> Result<Value, AnimationError> + Send + Sync>;

#[derive(Clone)]
pub struct ErasedProperty {
    pub id: Id,
    pub value_type: ValueType,
    dependencies: Vec<Id>,
    evaluate: ErasedCompute,
}

impl fmt::Debug for ErasedProperty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Property")
            .field("id", &self.id)
            .field("value_type", &self.value_type)
            .field("dependencies", &self.dependencies)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeMode {
    Clamp,
    Loop,
    PingPong,
    Continue,
}

#[derive(Clone, Debug)]
pub struct TimeTransform {
    pub offset: Seconds,
    pub scale: f64,
    pub mode: TimeMode,
    /// When present, normalized clip progress is mapped directly to source time.
    pub remap: Option<Curve<f64>>,
}

impl Default for TimeTransform {
    fn default() -> Self {
        Self {
            offset: 0.0,
            scale: 1.0,
            mode: TimeMode::Clamp,
            remap: None,
        }
    }
}

impl TimeTransform {
    pub fn map(
        &self,
        relative_time: Seconds,
        clip_duration: Seconds,
        source_duration: Seconds,
    ) -> Result<Seconds, AnimationError> {
        let raw = if let Some(remap) = &self.remap {
            let progress = if clip_duration == 0.0 {
                0.0
            } else {
                relative_time / clip_duration
            };
            remap.sample(progress, &0.0)?
        } else {
            relative_time * self.scale + self.offset
        };
        if !raw.is_finite() {
            return Err(AnimationError::NonFiniteTime);
        }
        Ok(match self.mode {
            TimeMode::Continue => raw,
            TimeMode::Clamp => raw.clamp(0.0, source_duration.max(0.0)),
            TimeMode::Loop if source_duration > 0.0 => positive_modulo(raw, source_duration),
            TimeMode::PingPong if source_duration > 0.0 => {
                let cycle = positive_modulo(raw, source_duration * 2.0);
                if cycle <= source_duration {
                    cycle
                } else {
                    source_duration * 2.0 - cycle
                }
            }
            TimeMode::Loop | TimeMode::PingPong => {
                return Err(AnimationError::ZeroDurationTimeMode)
            }
        })
    }
}

fn positive_modulo(value: f64, modulus: f64) -> f64 {
    ((value % modulus) + modulus) % modulus
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub id: Id,
    pub source: Arc<Composition>,
    pub start: Seconds,
    pub duration: Seconds,
    pub time: TimeTransform,
}

impl Clip {
    pub fn is_active(&self, parent_time: Seconds) -> bool {
        self.duration > 0.0 && parent_time >= self.start && parent_time < self.start + self.duration
    }
}

#[derive(Clone, Debug)]
pub struct Composition {
    pub id: Id,
    pub width: u32,
    pub height: u32,
    pub duration: Seconds,
    pub frame_rate: f64,
    pub properties: Vec<ErasedProperty>,
    pub clips: Vec<Clip>,
}

impl Composition {
    pub fn validate(&self) -> Result<(), AnimationError> {
        if self.id.is_empty() {
            return Err(AnimationError::InvalidComposition(
                "composition id must not be empty".into(),
            ));
        }
        if self.width == 0
            || self.height == 0
            || !self.duration.is_finite()
            || self.duration < 0.0
            || !self.frame_rate.is_finite()
            || self.frame_rate <= 0.0
        {
            return Err(AnimationError::InvalidComposition(format!(
                "composition '{}' has invalid dimensions or timing",
                self.id
            )));
        }
        for clip in &self.clips {
            if clip.id.is_empty()
                || !clip.start.is_finite()
                || !clip.duration.is_finite()
                || clip.duration < 0.0
            {
                return Err(AnimationError::InvalidClip(clip.id.clone()));
            }
            clip.source.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct CompiledNode {
    scoped_id: Id,
    scope: Id,
    value_type: ValueType,
    dependencies: Vec<Id>,
    evaluate: ErasedCompute,
}

impl fmt::Debug for CompiledNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledNode")
            .field("scoped_id", &self.scoped_id)
            .field("scope", &self.scope)
            .field("value_type", &self.value_type)
            .field("dependencies", &self.dependencies)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct CompiledClip {
    scope: Id,
    parent_scope: Id,
    clip: Clip,
}

#[derive(Clone, Debug)]
pub struct CompiledGraph {
    root_id: Id,
    nodes: Vec<CompiledNode>,
    clips: Vec<CompiledClip>,
    order: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimeContext {
    pub root_time: Seconds,
    pub local_times: BTreeMap<Id, Seconds>,
    pub frame: Option<u64>,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvaluationSnapshot {
    pub composition_id: Id,
    pub time: TimeContext,
    pub values: BTreeMap<Id, Value>,
}

#[derive(Default)]
pub struct DependencyEvaluator;

impl DependencyEvaluator {
    pub fn compile(&self, root: &Composition) -> Result<CompiledGraph, AnimationError> {
        root.validate()?;
        let mut nodes = Vec::new();
        let mut clips = Vec::new();
        collect_graph(root, &root.id, &mut nodes, &mut clips)?;
        let order = topological_order(&nodes)?;
        Ok(CompiledGraph {
            root_id: root.id.clone(),
            nodes,
            clips,
            order,
        })
    }

    pub fn evaluate(
        &self,
        graph: &CompiledGraph,
        root_time: Seconds,
        frame: Option<u64>,
        seed: u64,
    ) -> Result<EvaluationSnapshot, AnimationError> {
        if !root_time.is_finite() {
            return Err(AnimationError::NonFiniteTime);
        }
        let mut local_times = BTreeMap::from([(graph.root_id.clone(), root_time)]);
        for compiled in &graph.clips {
            let Some(parent_time) = local_times.get(&compiled.parent_scope).copied() else {
                continue;
            };
            if compiled.clip.is_active(parent_time) {
                let relative = parent_time - compiled.clip.start;
                let local = compiled.clip.time.map(
                    relative,
                    compiled.clip.duration,
                    compiled.clip.source.duration,
                )?;
                local_times.insert(compiled.scope.clone(), local);
            }
        }
        let mut values = BTreeMap::new();
        for &index in &graph.order {
            let node = &graph.nodes[index];
            let Some(local_time) = local_times.get(&node.scope).copied() else {
                continue;
            };
            let context = EvaluationContext { values: &values };
            let value = (node.evaluate)(&context, local_time)?;
            if value_type(&value) != node.value_type {
                return Err(AnimationError::TypeMismatch {
                    id: node.scoped_id.clone(),
                    expected: node.value_type,
                    actual: value_type(&value),
                });
            }
            if !finite_value(&value) {
                return Err(AnimationError::NonFiniteValue(node.scoped_id.clone()));
            }
            values.insert(node.scoped_id.clone(), value);
        }
        Ok(EvaluationSnapshot {
            composition_id: graph.root_id.clone(),
            time: TimeContext {
                root_time,
                local_times,
                frame,
                seed,
            },
            values,
        })
    }
}

fn collect_graph(
    composition: &Composition,
    scope: &str,
    nodes: &mut Vec<CompiledNode>,
    clips: &mut Vec<CompiledClip>,
) -> Result<(), AnimationError> {
    let mut ids = BTreeSet::new();
    for property in &composition.properties {
        if property.id.is_empty() || !ids.insert(property.id.clone()) {
            return Err(AnimationError::DuplicateProperty(property.id.clone()));
        }
        let scoped_id = scoped(scope, &property.id);
        nodes.push(CompiledNode {
            scoped_id,
            scope: scope.into(),
            value_type: property.value_type,
            dependencies: property
                .dependencies
                .iter()
                .map(|id| scoped(scope, id))
                .collect(),
            evaluate: scope_compute(scope, property.evaluate.clone()),
        });
    }
    let mut clip_ids = BTreeSet::new();
    for clip in &composition.clips {
        if !clip_ids.insert(clip.id.clone()) {
            return Err(AnimationError::DuplicateClip(clip.id.clone()));
        }
        let child_scope = format!("{scope}/{}", clip.id);
        clips.push(CompiledClip {
            scope: child_scope.clone(),
            parent_scope: scope.into(),
            clip: clip.clone(),
        });
        collect_graph(&clip.source, &child_scope, nodes, clips)?;
    }
    Ok(())
}

fn scope_compute(scope: &str, compute: ErasedCompute) -> ErasedCompute {
    let prefix = format!("{scope}::");
    Arc::new(move |context, time| {
        let local = context
            .values
            .iter()
            .filter_map(|(id, value)| {
                id.strip_prefix(&prefix)
                    .filter(|rest| !rest.contains("::"))
                    .map(|rest| (rest.to_string(), value.clone()))
            })
            .collect();
        compute(&EvaluationContext { values: &local }, time)
    })
}

fn scoped(scope: &str, id: &str) -> Id {
    format!("{scope}::{id}")
}

fn topological_order(nodes: &[CompiledNode]) -> Result<Vec<usize>, AnimationError> {
    let index: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.scoped_id.as_str(), i))
        .collect();
    let mut indegree = vec![0usize; nodes.len()];
    let mut outgoing = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for dependency in &node.dependencies {
            let Some(&source) = index.get(dependency.as_str()) else {
                return Err(AnimationError::MissingDependency(dependency.clone()));
            };
            indegree[i] += 1;
            outgoing[source].push(i);
        }
    }
    let mut ready: BTreeSet<(Id, usize)> = nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| indegree[*i] == 0)
        .map(|(i, node)| (node.scoped_id.clone(), i))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some((_, index)) = ready.pop_first() {
        order.push(index);
        for &target in &outgoing[index] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.insert((nodes[target].scoped_id.clone(), target));
            }
        }
    }
    if order.len() != nodes.len() {
        let cycle = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| indegree[*i] > 0)
            .map(|(_, node)| node.scoped_id.clone())
            .collect();
        return Err(AnimationError::DependencyCycle(cycle));
    }
    Ok(order)
}

pub trait Renderer<Target> {
    type Error;
    fn initialize(&mut self, composition: &Composition, target: Target) -> Result<(), Self::Error>;
    fn render(&mut self, snapshot: &EvaluationSnapshot) -> Result<(), Self::Error>;
    fn dispose(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnimationError {
    InvalidComposition(String),
    InvalidClip(Id),
    InvalidCurve(String),
    DuplicateProperty(Id),
    DuplicateClip(Id),
    MissingDependency(Id),
    DependencyCycle(Vec<Id>),
    TypeMismatch {
        id: Id,
        expected: ValueType,
        actual: ValueType,
    },
    UnsupportedInterpolation(ValueType),
    NonFiniteTime,
    NonFiniteValue(Id),
    ZeroDurationTimeMode,
}

impl fmt::Display for AnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for AnimationError {}

fn value_type(value: &Value) -> ValueType {
    match value {
        Value::Number(_) => ValueType::Number,
        Value::Bool(_) => ValueType::Bool,
        Value::Text(_) => ValueType::Text,
        Value::Vec2(_) => ValueType::Vec2,
        Value::Color(_) => ValueType::Color,
    }
}
fn finite_value(value: &Value) -> bool {
    match value {
        Value::Number(v) => v.is_finite(),
        Value::Vec2(v) => v.x.is_finite() && v.y.is_finite(),
        Value::Color(v) => v.r.is_finite() && v.g.is_finite() && v.b.is_finite() && v.a.is_finite(),
        Value::Bool(_) | Value::Text(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(id: &str, source: PropertySource<f64>) -> ErasedProperty {
        Property {
            id: id.into(),
            default_value: 0.0,
            source,
        }
        .erase()
    }

    fn composition(properties: Vec<ErasedProperty>) -> Composition {
        Composition {
            id: "root".into(),
            width: 1920,
            height: 1080,
            duration: 10.0,
            frame_rate: 30.0,
            properties,
            clips: vec![],
        }
    }

    #[test]
    fn curve_sampling_is_deterministic_and_typed() {
        let curve = Curve::new(vec![
            Keyframe {
                time: 0.0,
                value: 0.0,
                interpolation: Interpolation::Linear,
            },
            Keyframe {
                time: 2.0,
                value: 10.0,
                interpolation: Interpolation::Hold,
            },
        ])
        .unwrap();
        assert_eq!(curve.sample(1.0, &0.0).unwrap(), 5.0);
        assert_eq!(curve.sample(1.0, &0.0).unwrap(), 5.0);

        let mut extrapolated = curve.clone();
        extrapolated.pre_extrapolation = Extrapolation::Linear;
        extrapolated.post_extrapolation = Extrapolation::Linear;
        assert_eq!(extrapolated.sample(-1.0, &0.0).unwrap(), -5.0);
        assert_eq!(extrapolated.sample(3.0, &0.0).unwrap(), 15.0);

        let eased = Curve::new(vec![
            Keyframe {
                time: 0.0,
                value: 0.0,
                interpolation: Interpolation::CubicBezier(0.2, 0.8, 0.2, 1.0),
            },
            Keyframe {
                time: 1.0,
                value: 1.0,
                interpolation: Interpolation::Hold,
            },
        ])
        .unwrap();
        assert!(eased.sample(0.5, &0.0).unwrap() > 0.5);

        let bool_curve = Curve::new(vec![
            Keyframe {
                time: 0.0,
                value: false,
                interpolation: Interpolation::Linear,
            },
            Keyframe {
                time: 1.0,
                value: true,
                interpolation: Interpolation::Hold,
            },
        ])
        .unwrap();
        assert_eq!(
            bool_curve.sample(0.5, &false),
            Err(AnimationError::UnsupportedInterpolation(ValueType::Bool))
        );
    }

    #[test]
    fn curve_properties_are_evaluated_at_requested_time() {
        let curve = Curve::new(vec![
            Keyframe {
                time: 0.0,
                value: 2.0,
                interpolation: Interpolation::Linear,
            },
            Keyframe {
                time: 2.0,
                value: 6.0,
                interpolation: Interpolation::Hold,
            },
        ])
        .unwrap();
        let evaluator = DependencyEvaluator;
        let graph = evaluator
            .compile(&composition(vec![number(
                "animated",
                PropertySource::Curve(curve),
            )]))
            .unwrap();
        let snapshot = evaluator.evaluate(&graph, 1.0, Some(30), 0).unwrap();
        assert_eq!(snapshot.values["root::animated"], Value::Number(4.0));
    }

    #[test]
    fn dependencies_use_stable_topological_evaluation() {
        let base = number("base", PropertySource::Constant(4.0));
        let doubled = number(
            "doubled",
            PropertySource::Computed {
                dependencies: vec!["base".into()],
                evaluate: Arc::new(|context, _| Ok(context.get::<f64>("base")? * 2.0)),
            },
        );
        let evaluator = DependencyEvaluator;
        let graph = evaluator
            .compile(&composition(vec![doubled, base]))
            .unwrap();
        let first = evaluator.evaluate(&graph, 3.0, Some(90), 7).unwrap();
        let second = evaluator.evaluate(&graph, 3.0, Some(90), 7).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.values["root::doubled"], Value::Number(8.0));
    }

    #[test]
    fn dependency_cycles_are_rejected_at_compile_time() {
        let a = number("a", PropertySource::Reference("b".into()));
        let b = number("b", PropertySource::Reference("a".into()));
        let error = DependencyEvaluator
            .compile(&composition(vec![a, b]))
            .unwrap_err();
        assert!(matches!(error, AnimationError::DependencyCycle(_)));
    }

    #[test]
    fn clips_have_exclusive_ends_and_independent_local_time() {
        let source = Arc::new(Composition {
            id: "child".into(),
            width: 100,
            height: 100,
            duration: 2.0,
            frame_rate: 30.0,
            properties: vec![number(
                "time",
                PropertySource::Computed {
                    dependencies: vec![],
                    evaluate: Arc::new(|_, time| Ok(time)),
                },
            )],
            clips: vec![],
        });
        let clip = Clip {
            id: "instance".into(),
            source,
            start: 1.0,
            duration: 4.0,
            time: TimeTransform {
                offset: 0.0,
                scale: 0.5,
                mode: TimeMode::Clamp,
                remap: None,
            },
        };
        assert!(clip.is_active(1.0));
        assert!(!clip.is_active(5.0));
        let mut root = composition(vec![]);
        root.clips.push(clip);
        let evaluator = DependencyEvaluator;
        let graph = evaluator.compile(&root).unwrap();
        let snapshot = evaluator.evaluate(&graph, 3.0, None, 0).unwrap();
        assert_eq!(snapshot.values["root/instance::time"], Value::Number(1.0));
        let ended = evaluator.evaluate(&graph, 5.0, None, 0).unwrap();
        assert!(!ended.values.contains_key("root/instance::time"));
    }

    #[test]
    fn time_transform_supports_reverse_loop_and_ping_pong() {
        let reverse = TimeTransform {
            offset: 2.0,
            scale: -1.0,
            mode: TimeMode::Clamp,
            remap: None,
        };
        assert_eq!(reverse.map(0.5, 2.0, 2.0).unwrap(), 1.5);
        let looping = TimeTransform {
            offset: 0.0,
            scale: 1.0,
            mode: TimeMode::Loop,
            remap: None,
        };
        assert_eq!(looping.map(2.5, 4.0, 2.0).unwrap(), 0.5);
        let ping = TimeTransform {
            mode: TimeMode::PingPong,
            ..looping
        };
        assert_eq!(ping.map(2.5, 4.0, 2.0).unwrap(), 1.5);
    }
}
