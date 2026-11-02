use crate::{BoxKey, BoxKind, ItemBox};
use form_model::{Item, ItemKind};

fn finite(r: &crate::Rect) -> bool {
    r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite()
}

fn inside(inner: &crate::Rect, outer: &crate::Rect) -> bool {
    inner.x >= outer.x - 1.0
        && inner.y >= outer.y - 1.0
        && inner.x + inner.w <= outer.x + outer.w + 1.0
        && inner.y + inner.h <= outer.y + outer.h + 1.0
}

fn check_tree(
    boxes: &std::collections::HashMap<&str, &ItemBox>,
    items: &[Item],
    container: Option<(&str, &ItemBox)>,
    vertical: bool,
    errs: &mut Vec<(&'static str, String)>,
) {
    let mut prev: Option<&ItemBox> = None;
    for it in items {
        let b = boxes.get(it.name.as_str()).copied().filter(|b| !b.hidden);
        if let Some(b) = b {
            if let Some((cn, cb)) = container
                && !inside(&b.rect, &cb.rect)
            {
                errs.push((
                    "b",
                    format!(
                        "(б) `{}` {:?} outside `{cn}` {:?}",
                        it.name, b.rect, cb.rect
                    ),
                ));
            }
            if vertical {
                if let Some(p) = prev
                    && p.rect.y + p.rect.h > b.rect.y + 1.0
                {
                    errs.push((
                        "c",
                        format!(
                            "(в) `{}` {:?} overlaps previous sibling {:?}",
                            it.name, b.rect, p.rect
                        ),
                    ));
                }
                prev = Some(b);
            }
        }
        let is_container = matches!(
            it.kind,
            ItemKind::UsualGroup | ItemKind::Pages | ItemKind::Page | ItemKind::CommandBar
        );
        match b {
            Some(b) if is_container => {
                let v = match it.kind {
                    ItemKind::UsualGroup => it.props.group == Some(form_model::GroupKind::Vertical),
                    ItemKind::Page => {
                        it.props.group.is_none()
                            || it.props.group == Some(form_model::GroupKind::Vertical)
                    }
                    _ => false,
                };
                check_tree(boxes, &it.children, Some((it.name.as_str(), b)), v, errs);
            }
            Some(_) => {}
            None if b.is_none() && boxes.get(it.name.as_str()).is_none() => {
                check_tree(boxes, &it.children, container, vertical, errs);
            }
            None => {}
        }
    }
}

pub fn invariants(
    form: &form_model::FormDesc,
    layout: &crate::Layout,
) -> Vec<(&'static str, String)> {
    let mut errs = Vec::new();
    let w = layout.content.w;
    let mut by_name = std::collections::HashMap::new();
    for b in &layout.boxes {
        if let BoxKey::Item(n) = &b.key {
            by_name.insert(n.as_str(), b);
        }
        if b.hidden {
            continue;
        }
        let ok = finite(&b.rect)
            && b.rect.w >= 0.0
            && b.rect.h >= 0.0
            && b.title.as_ref().is_none_or(|t| finite(&t.rect))
            && b.input.as_ref().is_none_or(finite);
        if !ok {
            errs.push(("a", format!("(а) {:?} {:?}", b.key, b.rect)));
        }
        if !matches!(b.kind, BoxKind::Grid | BoxKind::Column) && b.rect.w > w - 32.0 + 1.0 {
            errs.push(("d", format!("(г) {:?} width {}", b.key, b.rect.w)));
        }
    }
    check_tree(&by_name, &form.items, None, true, &mut errs);
    errs
}
