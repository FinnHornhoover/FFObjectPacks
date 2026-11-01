use super::*;

impl Exporter<'_> {
    pub(super) fn animations(&mut self, scene: &mut Assembly) -> Result<Vec<Json>> {
        let index = self.index;
        let Assembly {
            writer,
            nodes,
            renderers,
            owners,
            animations,
            warnings,
            ..
        } = scene;
        let clip_nodes = Json::Array(
            writer.json["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|source| {
                    let mut node = json!({"name": source["name"]});
                    if let Some(children) = source.get("children") {
                        node["children"] = children.clone();
                    }
                    node
                })
                .collect(),
        );
        let scene_roots = writer.json["scenes"][0]["nodes"].clone();
        let mut files = serde_json::Map::new();
        let mut records = Vec::new();
        let mut exported = HashMap::<(u32, u32), String>::new();
        for (binding_id, target) in animations.iter().copied() {
            let binding = self
                .bindings
                .get(&binding_id)
                .context("Missing animation binding")?
                .clone();
            let mut list = binding["clips"].array().to_vec();
            if let Some(default) = binding["default_clip"].id()
                && !list.iter().any(|v| v.id() == Some(default))
            {
                list.push(default.into());
            }
            let mut entries = Vec::new();
            let mut default_index = None;
            for clip_id in list {
                let Some(clip_id) = clip_id.id() else {
                    entries.push(json!({"name":null}));
                    continue;
                };
                if Some(clip_id) == binding["default_clip"].id() {
                    default_index = Some(entries.len());
                }
                let result = (|| -> Result<(String, String, f64)> {
                    let clip = self.clip(clip_id)?;
                    if let Some(file) = exported.get(&(binding_id, clip_id)) {
                        return Ok((clip.name.clone(), file.clone(), clip.duration));
                    }
                    let mut cw = Document::new();
                    cw.json["nodes"] = clip_nodes.clone();
                    cw.json["scenes"][0]["nodes"] = scene_roots.clone();
                    for node in cw.json["nodes"].as_array_mut().unwrap() {
                        node.as_object_mut().unwrap().remove("mesh");
                        node.as_object_mut().unwrap().remove("skin");
                    }
                    let mut targets = HashMap::new();
                    for pair in binding["targets"].array() {
                        if let (Some(path), Some(t)) = (
                            pair[0].id(),
                            pair[1].id().and_then(|id| nodes.get(&id).copied()),
                        ) {
                            targets.insert(index.paths[path as usize].clone(), t);
                        }
                    }
                    for pair in binding["ambiguous_paths"].array() {
                        if let (Some(path), Some(t)) = (
                            pair[0].id(),
                            pair[1]
                                .array()
                                .first()
                                .and_then(Fields::id)
                                .and_then(|id| nodes.get(&id).copied()),
                        ) {
                            targets.insert(index.paths[path as usize].clone(), t);
                        }
                    }
                    let mut animation = json!({"name":clip.name,"channels":[],"samplers":[],"extras":{"duration":clip.duration,
                        "source_clip":clip_id,"binding":binding_id,"unresolved":finite_json(&binding["unresolved_paths"]),"ambiguous":finite_json(&binding["ambiguous_paths"])}});
                    let mut floats = serde_json::Map::new();
                    let mut time_cache = HashMap::new();
                    for track in &clip.tracks {
                        let Some(&node) = targets.get(&track.path) else {
                            continue;
                        };
                        let mut values: Vec<Vec<f64>> = clip
                            .times
                            .iter()
                            .map(|t| track.curve.sample(f64::from(*t)))
                            .collect();
                        let width = values[0].len();
                        if track.property == "rotation" {
                            let mut previous: Option<Vec<f64>> = None;
                            for value in &mut values {
                                let norm = value.iter().map(|v| v * v).sum::<f64>().sqrt();
                                if norm > 0.0 {
                                    for v in value.iter_mut() {
                                        *v /= norm;
                                    }
                                } else {
                                    *value = vec![0.0, 0.0, 0.0, 1.0];
                                }
                                value[1] = -value[1];
                                value[2] = -value[2];
                                if previous.as_ref().is_some_and(|p| {
                                    p.iter().zip(value.iter()).map(|(a, b)| a * b).sum::<f64>()
                                        < 0.0
                                }) {
                                    for v in value.iter_mut() {
                                        *v = -*v;
                                    }
                                }
                                previous = Some(value.clone());
                            }
                        } else if track.property == "translation" {
                            for v in &mut values {
                                v[0] = -v[0];
                            }
                        }
                        let constant = values.iter().all(|v| v == &values[0]);
                        if constant {
                            values.truncate(1);
                        }
                        let mut times = if constant {
                            vec![0.0]
                        } else {
                            clip.times.clone()
                        };
                        let mut tr = json!({});
                        let mut flat: Vec<f32> =
                            values.iter().flatten().map(|v| *v as f32).collect();
                        gltf::reduce::samples(
                            &mut times,
                            &mut flat,
                            width,
                            track.property == "rotation",
                        );
                        if flat.iter().all(|v| v.is_finite()) {
                            let key: Vec<_> = times.iter().map(|v| v.to_bits()).collect();
                            let input = if let Some(input) = time_cache.get(&key) {
                                *input
                            } else {
                                let input = cw.accessor(&times, 1, true)?;
                                time_cache.insert(key, input);
                                input
                            };
                            tr["input"] = input.into();
                            tr["output"] = cw.accessor(&flat, width, false)?.into();
                        } else {
                            tr["times"] = json!(times);
                            tr["values"] = json!(
                                flat.iter()
                                    .map(|v| if v.is_finite() { Some(*v) } else { None })
                                    .collect::<Vec<_>>()
                            );
                        }
                        if ["translation", "rotation", "scale"].contains(&track.property.as_str()) {
                            ensure!(tr.get("input").is_some(), "Nonfinite transform samples");
                            let samplers = animation["samplers"].as_array_mut().unwrap();
                            let sampler = samplers.len();
                            samplers.push(json!({"input":tr["input"],"output":tr["output"],"interpolation":"LINEAR"}));
                            animation["channels"].as_array_mut().unwrap().push(json!({"sampler":sampler,"target":{"node":node,"path":track.property}}));
                        } else {
                            let renderer_ids = owners.get(&node).cloned().unwrap_or_default();
                            let renderer_nodes: Vec<_> = renderer_ids
                                .iter()
                                .flat_map(|id| renderers.get(id).into_iter().flatten().copied())
                                .collect();
                            tr["node"] = renderer_nodes.first().copied().unwrap_or(node).into();
                            tr["source_node"] = node.into();
                            tr["renderers"] = json!(renderer_ids);
                            tr["renderer_nodes"] = json!(renderer_nodes);
                            tr["class_id"] = track.class.into();
                            tr["attribute"] = track.property.clone().into();
                            floats.insert(format!("{}:{}", track.path, track.property), tr);
                        }
                    }
                    animation["extras"]["float_curves"] = Json::Object(floats);
                    cw.json["extras"] = json!({"clip":{"name":clip.name,"duration":clip.duration,"source_clip":clip_id,"binding":binding_id}});
                    if animation["channels"].as_array().unwrap().is_empty() {
                        let mut mc = animation["extras"].clone();
                        mc["name"] = clip.name.clone().into();
                        cw.json["extras"]["material_clips"] = json!([mc]);
                    } else {
                        cw.add("animations", animation);
                    }
                    let file = cw.save(self.output, "animations/gltf")?;
                    exported.insert((binding_id, clip_id), file.clone());
                    Ok((clip.name.clone(), file, clip.duration))
                })();
                match result {
                    Ok((name, file, duration)) => {
                        let key = if files.contains_key(&name) {
                            format!("{name}@{binding_id}-{clip_id}")
                        } else {
                            name.clone()
                        };
                        files.insert(key, gltf::relative("models/prefabs", &file).into());
                        entries.push(json!({"name":name,"source_clip":clip_id,"file":gltf::relative("models/prefabs",&file)}));
                        records.push(json!({"name":name,"object":clip_id,"binding":binding_id,"file":file,"duration":duration}));
                    }
                    Err(error) => {
                        warnings.push(format!("Clip {clip_id}, binding {binding_id}: {error:#}"));
                        entries.push(json!({"name":null,"source_clip":clip_id}));
                    }
                }
            }
            let settings = &binding["settings"];
            let wrap = match settings["m_WrapMode"].integer() {
                0 => json!("Default"),
                1 => json!("Once"),
                2 => json!("Loop"),
                4 => json!("PingPong"),
                8 => json!("ClampForever"),
                v => json!(v),
            };
            writer.json["nodes"][target]["extras"]["animation"] = json!({"clips":entries,"default":default_index,"wrap_mode":wrap,
                "enabled":settings["m_Enabled"].is_nil() || settings["m_Enabled"].truth(),
                "play_automatically":settings["m_PlayAutomatically"].is_nil() || settings["m_PlayAutomatically"].truth(),"binding":binding_id});
        }
        writer.json["extras"] = json!({"clips":files});
        Ok(records)
    }
}
