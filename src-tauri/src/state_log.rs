//! Compact state deltas for durable checkpoints without storing the whole ranking history per vote.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub path: Vec<String>,
    pub value: Value,
    pub remove: bool,
}

pub fn diff(before: &Value, after: &Value) -> Vec<Change> {
    fn visit(before: &Value, after: &Value, path: &mut Vec<String>, out: &mut Vec<Change>) {
        if before == after {
            return;
        }
        match (before, after) {
            (Value::Object(a), Value::Object(b)) => {
                for key in a.keys().filter(|k| !b.contains_key(*k)) {
                    path.push(key.clone());
                    out.push(Change {
                        path: path.clone(),
                        value: Value::Null,
                        remove: true,
                    });
                    path.pop();
                }
                for (key, value) in b {
                    path.push(key.clone());
                    match a.get(key) {
                        Some(old) => visit(old, value, path, out),
                        None => out.push(Change {
                            path: path.clone(),
                            value: value.clone(),
                            remove: false,
                        }),
                    }
                    path.pop();
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                for i in 0..a.len().min(b.len()) {
                    path.push(i.to_string());
                    visit(&a[i], &b[i], path, out);
                    path.pop();
                }
                for i in (b.len()..a.len()).rev() {
                    path.push(i.to_string());
                    out.push(Change {
                        path: path.clone(),
                        value: Value::Null,
                        remove: true,
                    });
                    path.pop();
                }
                for (i, value) in b.iter().enumerate().skip(a.len()) {
                    path.push(i.to_string());
                    out.push(Change {
                        path: path.clone(),
                        value: value.clone(),
                        remove: false,
                    });
                    path.pop();
                }
            }
            _ => out.push(Change {
                path: path.clone(),
                value: after.clone(),
                remove: false,
            }),
        }
    }
    let mut changes = Vec::new();
    visit(before, after, &mut Vec::new(), &mut changes);
    changes
}

pub fn apply(value: &mut Value, changes: &[Change]) -> Result<(), String> {
    for change in changes {
        let Some((last, parents)) = change.path.split_last() else {
            if change.remove {
                return Err("Cannot remove session root".into());
            }
            *value = change.value.clone();
            continue;
        };
        let mut target = &mut *value;
        for part in parents {
            target = match target {
                Value::Object(map) => map.get_mut(part),
                Value::Array(array) => part.parse::<usize>().ok().and_then(|i| array.get_mut(i)),
                _ => None,
            }
            .ok_or("Invalid persisted delta path")?;
        }
        match target {
            Value::Object(map) => {
                if change.remove {
                    if map.remove(last).is_none() {
                        return Err("Invalid persisted removal".into());
                    }
                } else {
                    map.insert(last.clone(), change.value.clone());
                }
            }
            Value::Array(array) => {
                let i = last.parse::<usize>().map_err(|_| "Invalid delta index")?;
                if change.remove {
                    if i >= array.len() {
                        return Err("Delta index exceeds array".into());
                    }
                    array.remove(i);
                } else if i == array.len() {
                    array.push(change.value.clone());
                } else if i < array.len() {
                    array[i] = change.value.clone();
                } else {
                    return Err("Delta index gap".into());
                }
            }
            _ => return Err("Delta parent is not a collection".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn deltas_roundtrip_nested_changes_and_array_branches() {
        let a = json!({"run":{"history":[1,2,3],"pair":["a","b"]},"selected":["a","b"],"obsolete":true});
        let b =
            json!({"run":{"history":[1,9],"pair":["b","c"]},"selected":["a","b","c"],"new":false});
        let changes = diff(&a, &b);
        let mut restored = a.clone();
        apply(&mut restored, &changes).unwrap();
        assert_eq!(restored, b);
        apply(&mut restored, &diff(&b, &a)).unwrap();
        assert_eq!(restored, a);
    }
    #[test]
    fn appended_decision_does_not_repeat_full_history() {
        let mut a = json!({"events":(0..1000).collect::<Vec<_>>()});
        let mut b = a.clone();
        b["events"].as_array_mut().unwrap().push(json!(1000));
        let changes = diff(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(serde_json::to_vec(&changes).unwrap().len() < 100);
        apply(&mut a, &changes).unwrap();
        assert_eq!(a, b);
    }
    #[test]
    fn invalid_paths_are_rejected() {
        let mut value = json!({"items":[]});
        assert!(apply(
            &mut value,
            &[Change {
                path: vec!["items".into(), "4".into()],
                value: json!(1),
                remove: false
            }]
        )
        .is_err());
    }
}
