// Modified by enzUI contributors. See FORK.md.
pub(crate) fn collect_if_ready<C: ?Sized, T>(
    active: bool,
    context: &mut C,
    prepare: impl FnOnce(&mut C) -> bool,
    collect: impl FnOnce(&mut C) -> Option<T>,
) -> Option<T> {
    if !active || !prepare(context) {
        return None;
    }
    collect(context)
}

#[cfg(test)]
mod tests {
    use super::collect_if_ready;

    #[test]
    fn accessibility_publication_inactive_skips_preparation_and_collection() {
        let mut calls = Vec::new();
        assert_eq!(
            collect_if_ready(
                false,
                &mut calls,
                |calls| {
                    calls.push("prepare");
                    true
                },
                |calls| {
                    calls.push("collect");
                    Some(1)
                }
            ),
            None,
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn accessibility_publication_unconfirmed_geometry_defers_collection() {
        let mut calls = Vec::new();
        assert_eq!(
            collect_if_ready(
                true,
                &mut calls,
                |calls| {
                    calls.push("prepare");
                    false
                },
                |calls| {
                    calls.push("collect");
                    Some(1)
                }
            ),
            None,
        );
        assert_eq!(calls, ["prepare"]);
    }

    #[test]
    fn accessibility_publication_confirmed_geometry_prepares_once_before_collection() {
        let mut calls = Vec::new();
        assert_eq!(
            collect_if_ready(
                true,
                &mut calls,
                |calls| {
                    calls.push("prepare");
                    true
                },
                |calls| {
                    calls.push("collect");
                    Some(7)
                }
            ),
            Some(7),
        );
        assert_eq!(calls, ["prepare", "collect"]);
    }

    #[test]
    fn accessibility_publication_empty_builder_remains_unpublished() {
        let mut calls = Vec::new();
        let result: Option<()> = collect_if_ready(
            true,
            &mut calls,
            |calls| {
                calls.push("prepare");
                true
            },
            |calls| {
                calls.push("collect");
                None
            },
        );
        assert_eq!(result, None);
        assert_eq!(calls, ["prepare", "collect"]);
    }
}
