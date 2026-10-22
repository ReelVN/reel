//! Geometry records use one field schema for construction and author declarations.
use crate::aside::{AdapterError, observable::SchemaKeys, source::HostTypes};
use aside_host::{
    Properties, PropertyDocumentation, PropertySchema,
    source::{Api, Member, NamedType, named, nullable},
};
use aside_vm::{Heap, Value};

#[derive(Properties)]
#[expect(dead_code, reason = "Variants define the generated record schema.")]
pub(super) enum Point {
    /// Horizontal coordinate in the query's reference space.
    #[aside(ty=Float, readonly)]
    X,
    /// Vertical coordinate in the query's reference space.
    #[aside(ty=Float, readonly)]
    Y,
}

#[derive(Properties)]
#[expect(dead_code, reason = "Variants define the generated record schema.")]
pub(super) enum Bounds {
    /// Left edge of the transformed border box in reference coordinates.
    #[aside(ty=Float, readonly)]
    X,
    /// Top edge of the transformed border box in reference coordinates.
    #[aside(ty=Float, readonly)]
    Y,
    /// Width of the transformed axis-aligned border box.
    #[aside(ty=Float, readonly)]
    Width,
    /// Height of the transformed axis-aligned border box.
    #[aside(ty=Float, readonly)]
    Height,
}

#[derive(Properties)]
#[expect(dead_code, reason = "Variants define the generated record schema.")]
pub(super) enum Transform {
    /// Local x contribution to the reference x coordinate.
    #[aside(ty=Float, readonly)]
    A,
    /// Local x contribution to the reference y coordinate.
    #[aside(ty=Float, readonly)]
    B,
    /// Local y contribution to the reference x coordinate.
    #[aside(ty=Float, readonly)]
    C,
    /// Local y contribution to the reference y coordinate.
    #[aside(ty=Float, readonly)]
    D,
    /// Horizontal translation in reference coordinates.
    #[aside(ty=Float, readonly)]
    Tx,
    /// Vertical translation in reference coordinates.
    #[aside(ty=Float, readonly)]
    Ty,
}

#[derive(Properties)]
#[expect(dead_code, reason = "Variants define the generated record schema.")]
pub(super) enum Geometry {
    /// Local layout width before the reference transform.
    #[aside(ty=Float, readonly)]
    Width,
    /// Local layout height before the reference transform.
    #[aside(ty=Float, readonly)]
    Height,
    /// Axis-aligned bounds of the transformed border box, without clipping.
    #[aside(ty=Any, doc_type=named("reel::types::Bounds"), readonly)]
    Bounds,
    /// Affine transform from target-local points to reference coordinates.
    #[aside(ty=Any, doc_type=named("reel::types::Transform"), readonly)]
    Transform,
    /// Lower endpoint of the complete text's final caret, or null for absent text.
    #[aside(name="text_end", ty=Any, doc_type=nullable(named("reel::types::Point")), readonly)]
    TextEnd,
}

pub(super) trait Record: PropertySchema {
    const NAME: &'static str;
    const DOCUMENTATION: &'static str;
}
impl Record for Point {
    const NAME: &'static str = "reel::types::Point";
    const DOCUMENTATION: &'static str =
        "A read-only point in the geometry query's reference space.";
}
impl Record for Bounds {
    const NAME: &'static str = "reel::types::Bounds";
    const DOCUMENTATION: &'static str = "Read-only axis-aligned bounds in reference coordinates.";
}
impl Record for Transform {
    const NAME: &'static str = "reel::types::Transform";
    const DOCUMENTATION: &'static str =
        "A read-only affine transform: x'=a*x+c*y+tx; y'=b*x+d*y+ty.";
}
impl Record for Geometry {
    const NAME: &'static str = "reel::types::Geometry";
    const DOCUMENTATION: &'static str =
        "The immutable geometry captured from one accepted presentation.";
}

fn describe_record<T: Record>(api: &mut Api) {
    let fields = T::PROPERTIES
        .iter()
        .enumerate()
        .map(|(index, property)| {
            let mut member = Member::property(
                T::NAME,
                PropertyDocumentation {
                    property,
                    markdown: T::documentation(index),
                },
            );
            member.declaration.property_default = None;
            member
        })
        .collect();
    api.types
        .push(NamedType::new(T::NAME, T::DOCUMENTATION, fields));
}

pub(in crate::aside) fn describe(api: &mut Api) {
    describe_record::<Point>(api);
    describe_record::<Bounds>(api);
    describe_record::<Transform>(api);
    describe_record::<Geometry>(api);
    let presentation = api
        .types
        .iter()
        .find(|ty| ty.name == "reel::types::Presentation")
        .unwrap();
    let mut request = NamedType::new(
        "reel::types::GeometryRequest",
        "An owned request for geometry from the next accepted presentation.",
        Vec::new(),
    )
    .extends(presentation);
    request.await_result = Some(named(Geometry::NAME));
    api.types.push(request);
    api.refine(
        "reel::stage::geometry",
        named("reel::types::GeometryRequest"),
        &[
            ("node", named("reel::types::Node")),
            ("relative_to", nullable(named("reel::types::Node"))),
        ],
    );
}

pub(super) fn record<T: Record, const N: usize>(
    heap: &mut Heap,
    types: &HostTypes,
    keys: &mut SchemaKeys,
    values: [Value; N],
) -> Result<Value, AdapterError> {
    debug_assert_eq!(N, T::NAMES.len());
    let fields = std::array::from_fn::<_, N, _>(|index| (T::NAMES[index], values[index]));
    let value = keys.record(heap, &fields, &[])?;
    types.mark(heap, value, T::NAME)?;
    Ok(value)
}
