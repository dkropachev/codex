use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Value;

const MAX_SCHEMA_TRAVERSAL_DEPTH: usize = 32;
const MAX_SCHEMA_TRAVERSAL_NODES: usize = 2_048;
const MAX_PROPERTY_VARIANTS: usize = 256;
const MAX_VALUE_CANDIDATES: usize = 512;

type PropertyVariants<'a> = Vec<Vec<&'a Value>>;
type SchemaShape<'a> = BTreeMap<String, PropertyVariants<'a>>;

pub(crate) struct StaticProperty<'a> {
    variants: PropertyVariants<'a>,
}

impl<'a> StaticProperty<'a> {
    pub(crate) fn description(&self, root: &'a Value) -> Option<&'a str> {
        let mut descriptions = BTreeSet::new();
        let mut budget = TraversalBudget::new();
        for schema in self.variants.iter().flatten() {
            collect_descriptions(
                root,
                schema,
                &mut BTreeSet::new(),
                &mut budget,
                /*depth*/ 0,
                &mut descriptions,
            );
            if descriptions.len() > 1 {
                return None;
            }
        }
        descriptions.into_iter().next()
    }

    pub(crate) fn values(&self, root: &'a Value) -> Vec<&'a Value> {
        let mut values = Vec::new();
        let mut budget = TraversalBudget::new();
        for variant in &self.variants {
            let mut constraint = ValueConstraint::Unspecified;
            for schema in variant {
                constraint = intersect_constraints(
                    constraint,
                    value_constraint(
                        root,
                        schema,
                        &mut BTreeSet::new(),
                        &mut budget,
                        /*depth*/ 0,
                    ),
                );
            }
            if let ValueConstraint::Finite(candidates) = constraint {
                append_unique_values(&mut values, candidates);
            }
        }
        values
    }
}

pub(crate) fn top_level_properties(root: &Value) -> BTreeMap<String, StaticProperty<'_>> {
    let mut budget = TraversalBudget::new();
    collect_shape(
        root,
        root,
        &mut BTreeSet::new(),
        &mut budget,
        /*depth*/ 0,
    )
    .into_iter()
    .map(|(name, variants)| (name, StaticProperty { variants }))
    .collect()
}

struct TraversalBudget {
    remaining_nodes: usize,
}

impl TraversalBudget {
    fn new() -> Self {
        Self {
            remaining_nodes: MAX_SCHEMA_TRAVERSAL_NODES,
        }
    }

    fn enter(&mut self, depth: usize) -> bool {
        if depth >= MAX_SCHEMA_TRAVERSAL_DEPTH || self.remaining_nodes == 0 {
            return false;
        }
        self.remaining_nodes -= 1;
        true
    }
}

fn collect_shape<'a>(
    root: &'a Value,
    schema: &'a Value,
    active_refs: &mut BTreeSet<String>,
    budget: &mut TraversalBudget,
    depth: usize,
) -> SchemaShape<'a> {
    if !budget.enter(depth) {
        return BTreeMap::new();
    }
    let Some(schema) = schema.as_object() else {
        return BTreeMap::new();
    };
    let mut shape = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.iter())
        .map(|(name, property)| (name.clone(), vec![vec![property]]))
        .collect();

    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && active_refs.insert(reference.to_string())
    {
        if let Some(referenced) = resolve_local_ref(root, reference, budget, depth + 1) {
            and_shape(
                &mut shape,
                collect_shape(root, referenced, active_refs, budget, depth + 1),
            );
        }
        active_refs.remove(reference);
    }
    for branch in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        and_shape(
            &mut shape,
            collect_shape(root, branch, active_refs, budget, depth + 1),
        );
    }
    for keyword in ["anyOf", "oneOf"] {
        let mut alternatives = BTreeMap::new();
        for branch in schema
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            or_shape(
                &mut alternatives,
                collect_shape(root, branch, active_refs, budget, depth + 1),
            );
        }
        and_shape(&mut shape, alternatives);
    }
    shape
}

fn and_shape<'a>(left: &mut SchemaShape<'a>, right: SchemaShape<'a>) {
    for (name, right_variants) in right {
        let Some(left_variants) = left.get_mut(&name) else {
            left.insert(name, right_variants);
            continue;
        };
        let mut merged = Vec::new();
        for left_variant in left_variants.iter() {
            for right_variant in &right_variants {
                let mut variant = left_variant.clone();
                variant.extend(right_variant);
                merged.push(variant);
                if merged.len() == MAX_PROPERTY_VARIANTS {
                    break;
                }
            }
            if merged.len() == MAX_PROPERTY_VARIANTS {
                break;
            }
        }
        *left_variants = merged;
    }
}

fn or_shape<'a>(left: &mut SchemaShape<'a>, right: SchemaShape<'a>) {
    for (name, variants) in right {
        let retained = left.entry(name).or_default();
        retained.extend(
            variants
                .into_iter()
                .take(MAX_PROPERTY_VARIANTS.saturating_sub(retained.len())),
        );
    }
}

#[derive(Debug)]
enum ValueConstraint<'a> {
    Impossible,
    Unspecified,
    Finite(Vec<&'a Value>),
}

fn value_constraint<'a>(
    root: &'a Value,
    schema: &'a Value,
    active_refs: &mut BTreeSet<String>,
    budget: &mut TraversalBudget,
    depth: usize,
) -> ValueConstraint<'a> {
    if !budget.enter(depth) {
        return ValueConstraint::Unspecified;
    }
    if schema == &Value::Bool(false) {
        return ValueConstraint::Impossible;
    }
    let Some(schema) = schema.as_object() else {
        return ValueConstraint::Unspecified;
    };
    let mut constraint = ValueConstraint::Unspecified;
    if let Some(value) = schema.get("const") {
        constraint = ValueConstraint::Finite(vec![value]);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        constraint = intersect_constraints(
            constraint,
            ValueConstraint::Finite(unique_values(values.iter())),
        );
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && active_refs.insert(reference.to_string())
    {
        if let Some(referenced) = resolve_local_ref(root, reference, budget, depth + 1) {
            constraint = intersect_constraints(
                constraint,
                value_constraint(root, referenced, active_refs, budget, depth + 1),
            );
        }
        active_refs.remove(reference);
    }
    for branch in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        constraint = intersect_constraints(
            constraint,
            value_constraint(root, branch, active_refs, budget, depth + 1),
        );
    }
    for keyword in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let alternatives = branches
                .iter()
                .map(|branch| value_constraint(root, branch, active_refs, budget, depth + 1))
                .collect();
            constraint = intersect_constraints(constraint, union_constraints(alternatives));
        }
    }
    constraint
}

fn intersect_constraints<'a>(
    left: ValueConstraint<'a>,
    right: ValueConstraint<'a>,
) -> ValueConstraint<'a> {
    match (left, right) {
        (ValueConstraint::Impossible, _) | (_, ValueConstraint::Impossible) => {
            ValueConstraint::Impossible
        }
        (ValueConstraint::Unspecified, constraint) | (constraint, ValueConstraint::Unspecified) => {
            constraint
        }
        (ValueConstraint::Finite(left), ValueConstraint::Finite(right)) => {
            let values = left
                .into_iter()
                .filter(|candidate| right.contains(candidate))
                .collect::<Vec<_>>();
            if values.is_empty() {
                ValueConstraint::Impossible
            } else {
                ValueConstraint::Finite(values)
            }
        }
    }
}

fn union_constraints<'a>(constraints: Vec<ValueConstraint<'a>>) -> ValueConstraint<'a> {
    let mut values = Vec::new();
    let mut has_possible_branch = false;
    for constraint in constraints {
        match constraint {
            ValueConstraint::Impossible => {}
            ValueConstraint::Unspecified => has_possible_branch = true,
            ValueConstraint::Finite(candidates) => {
                has_possible_branch = true;
                append_unique_values(&mut values, candidates);
            }
        }
    }
    if !values.is_empty() {
        ValueConstraint::Finite(values)
    } else if has_possible_branch {
        ValueConstraint::Unspecified
    } else {
        ValueConstraint::Impossible
    }
}

fn unique_values<'a>(values: impl IntoIterator<Item = &'a Value>) -> Vec<&'a Value> {
    let mut unique = Vec::new();
    append_unique_values(&mut unique, values);
    unique
}

fn append_unique_values<'a>(
    retained: &mut Vec<&'a Value>,
    values: impl IntoIterator<Item = &'a Value>,
) {
    for value in values {
        if retained.len() == MAX_VALUE_CANDIDATES {
            break;
        }
        if !retained.contains(&value) {
            retained.push(value);
        }
    }
}

fn collect_descriptions<'a>(
    root: &'a Value,
    schema: &'a Value,
    active_refs: &mut BTreeSet<String>,
    budget: &mut TraversalBudget,
    depth: usize,
    descriptions: &mut BTreeSet<&'a str>,
) {
    if descriptions.len() > 1 || !budget.enter(depth) {
        return;
    }
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(description) = schema.get("description").and_then(Value::as_str) {
        descriptions.insert(description);
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && active_refs.insert(reference.to_string())
    {
        if let Some(referenced) = resolve_local_ref(root, reference, budget, depth + 1) {
            collect_descriptions(
                root,
                referenced,
                active_refs,
                budget,
                depth + 1,
                descriptions,
            );
        }
        active_refs.remove(reference);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        for branch in schema
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_descriptions(root, branch, active_refs, budget, depth + 1, descriptions);
        }
    }
}

fn resolve_local_ref<'a>(
    root: &'a Value,
    reference: &str,
    budget: &mut TraversalBudget,
    depth: usize,
) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let fragment = reference.strip_prefix('#')?;
    if fragment.starts_with('/') {
        root.pointer(fragment)
    } else {
        find_anchor(root, fragment, budget, depth)
    }
}

fn find_anchor<'a>(
    schema: &'a Value,
    anchor: &str,
    budget: &mut TraversalBudget,
    depth: usize,
) -> Option<&'a Value> {
    if !budget.enter(depth) {
        return None;
    }
    match schema {
        Value::Object(object) => {
            if ["$anchor", "$dynamicAnchor"]
                .into_iter()
                .any(|keyword| object.get(keyword).and_then(Value::as_str) == Some(anchor))
            {
                return Some(schema);
            }
            object
                .values()
                .find_map(|value| find_anchor(value, anchor, budget, depth + 1))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_anchor(value, anchor, budget, depth + 1)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}
