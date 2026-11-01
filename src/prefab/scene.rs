use super::*;

impl Exporter<'_> {
    pub(super) fn scene(&mut self, root: u32) -> Result<Assembly> {
        let index = self.index;
        let transform = index
            .transform(root)
            .context("Prefab root has no Transform")?;
        let ids = index.hierarchy(transform);
        let nodes: HashMap<_, _> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut writer = Document::new();
        let mut warnings = Vec::<String>::new();
        let mut renderers = BTreeMap::<u32, Vec<usize>>::new();
        let mut owners = HashMap::<usize, Vec<u32>>::new();
        let mut animations = Vec::new();
        for id in &ids {
            let node = &index.nodes[id];
            let d = &node.data;
            let go = d["go"].id().and_then(|id| index.nodes.get(&id));
            let q = &d["q"].array();
            let norm = q.iter().map(|v| v.number().powi(2)).sum::<f64>().sqrt();
            let rotation = if norm > 0.0 {
                vec![
                    q[0].number() / norm,
                    -q[1].number() / norm,
                    -q[2].number() / norm,
                    q[3].number() / norm,
                ]
            } else {
                vec![0.0, 0.0, 0.0, 1.0]
            };
            let children: Vec<_> = d["children"]
                .array()
                .iter()
                .filter_map(Fields::id)
                .filter_map(|id| nodes.get(&id).copied())
                .collect();
            let mut out = json!({"name":go.map(|n|n.data["name"].text()).unwrap_or(""),
                "translation":[-d["p"][0].number(),d["p"][1].number(),d["p"][2].number()],
                "rotation":rotation,"scale":d["s"].array().iter().map(Fields::number).collect::<Vec<_>>(),
                "extras":{"source_transform":id,"source_object":d["go"].id(),"active":go.map(|n|n.data["active"].truth()).unwrap_or(true)}});
            if !children.is_empty() {
                out["children"] = json!(children);
            }
            writer.add("nodes", out);
        }
        // Add external joint chains that were not children of the exported root.
        for id in &ids {
            let node = &index.nodes[id];
            if let Some(parent) = node.data["parent"]
                .id()
                .and_then(|p| nodes.get(&p).copied())
            {
                let child = nodes[id];
                let children = writer.json["nodes"][parent]
                    .as_object_mut()
                    .unwrap()
                    .entry("children")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .unwrap();
                if !children.contains(&json!(child)) {
                    children.push(child.into());
                }
            } else {
                writer.json["scenes"][0]["nodes"]
                    .as_array_mut()
                    .unwrap()
                    .push(nodes[id].into());
            }
        }
        let mut rig_count = 0;
        for id in &ids {
            let node = &index.nodes[id];
            let Some(go) = node.data["go"].id().and_then(|id| index.nodes.get(&id)) else {
                continue;
            };
            let target = nodes[id];
            let mut bone_table = None;
            for c in go.data["components"].array() {
                let Some(component_id) = c[1].id() else {
                    continue;
                };
                let Some(component) = index.nodes.get(&component_id) else {
                    continue;
                };
                match component.kind {
                    111 => animations.push((component_id, target)),
                    23 | 137 => {}
                    _ => {
                        if component.data["script_type"].text() == "ActorSkinCombiner" {
                            let bones: Vec<_> = component.data["actorBones"]
                                .array()
                                .iter()
                                .map(|r| {
                                    r["r"]
                                        .id()
                                        .and_then(|id| nodes.get(&id).copied())
                                        .map(|n| n as i64)
                                        .unwrap_or(-1)
                                })
                                .collect();
                            writer.json["nodes"][target]["extras"]["actor_bones"] = json!(bones);
                            rig_count += 1;
                        }
                        if component.data["script_type"].text() == "ActorWearIndexTable" {
                            bone_table = Some(&component.data);
                        }
                    }
                }
            }
            for c in go.data["components"].array() {
                let Some(renderer_id) = c[1].id() else {
                    continue;
                };
                let Some(renderer) = index
                    .nodes
                    .get(&renderer_id)
                    .filter(|n| [23, 137].contains(&n.kind))
                else {
                    continue;
                };
                if !renderer.data["m_Enabled"].is_nil() && !renderer.data["m_Enabled"].truth() {
                    continue;
                }
                let Some(mesh_id) = index.renderer_mesh(go, renderer) else {
                    continue;
                };
                let Some(mesh) = self.plan.objects.get(&mesh_id) else {
                    warnings.push(format!("Renderer {renderer_id}: mesh {mesh_id} is missing"));
                    continue;
                };
                if mesh.data["source_empty"].truth() {
                    continue;
                }
                if mesh.data["invalid_uv_values"].integer() > 0 {
                    warnings.push(format!(
                        "Renderer {renderer_id}: replaced {} nonfinite source UV values with zero",
                        mesh.data["invalid_uv_values"].integer()
                    ));
                }
                if !self.geometry.contains_key(&mesh_id) {
                    self.geometry.insert(
                        mesh_id,
                        gltf::read(&self.output.join(mesh.data["file"].text()))?,
                    );
                }
                let mut primitives = writer.geometry(&self.geometry[&mesh_id])?;
                for primitive in &mut primitives {
                    let slot = primitive["material"].as_u64().unwrap_or(0) as usize;
                    let material = renderer.data["m_Materials"][slot]["r"].id();
                    primitive["material"] = writer.material(material, &self.plan.objects)?.into();
                }
                let mut output_skin = None;
                if renderer.kind == 137 && !renderer.data["m_Bones"].array().is_empty() {
                    let skin_result = (|| -> Result<usize> {
                        let bones: Vec<_> = renderer.data["m_Bones"]
                            .array()
                            .iter()
                            .map(|r| r["r"].id().and_then(|id| nodes.get(&id).copied()))
                            .collect();
                        ensure!(
                            bones.iter().all(Option::is_some),
                            "Some renderer bones are outside available hierarchy"
                        );
                        let joints: Vec<_> = bones.into_iter().map(Option::unwrap).collect();
                        let raw =
                            unpack(&fs::read(self.output.join(mesh.data["skin_data"].text()))?)?;
                        let (joint_values, weights) = skin::weights(
                            &raw,
                            mesh.data["vertices"].integer() as usize,
                            joints.len(),
                        )?;
                        let js = writer.joints(&joint_values);
                        let ws = writer.accessor(&weights, 4, false)?;
                        for p in &mut primitives {
                            p["attributes"]["JOINTS_0"] = js.into();
                            p["attributes"]["WEIGHTS_0"] = ws.into();
                        }
                        let stored = raw["m_BindPose"].array();
                        let mesh_world = index.world(*id, &nodes)?;
                        let mut matrices = Vec::new();
                        for (i, joint) in joints.iter().enumerate() {
                            let bind = if stored.len() == joints.len() {
                                skin::bind(&stored[i])
                            } else {
                                let joint_world = index.world(ids[*joint], &nodes)?;
                                ensure!(
                                    joint_world.determinant().abs() > 1e-20,
                                    "Singular joint bind transform"
                                );
                                joint_world.inverse() * mesh_world
                            };
                            ensure!(bind.is_finite(), "Invalid inverse bind matrix");
                            matrices.extend(skin::mirror(bind).to_cols_array().map(|n| n as f32));
                        }
                        let ibm = writer.accessor(&matrices, 16, false)?;
                        let mut skin = json!({"joints":joints,"inverseBindMatrices":ibm});
                        if let Some(table) = bone_table {
                            let mapping = |name: &str| {
                                table[name]
                                    .array()
                                    .iter()
                                    .map(Fields::integer)
                                    .collect::<Vec<_>>()
                            };
                            skin["extras"] = json!({"actor_bones":{"m":mapping("transformIndicesM"),"f":mapping("transformIndicesF")}});
                        }
                        Ok(writer.add("skins", skin))
                    })();
                    match skin_result {
                        Ok(skin) => output_skin = Some(skin),
                        Err(error) => {
                            for p in &mut primitives {
                                p["attributes"].as_object_mut().unwrap().remove("JOINTS_0");
                                p["attributes"].as_object_mut().unwrap().remove("WEIGHTS_0");
                            }
                            warnings.push(format!(
                                "Renderer {renderer_id}: exported unskinned: {error:#}"
                            ));
                        }
                    }
                }
                let mesh = writer.add(
                    "meshes",
                    json!({"name":writer.json["nodes"][target]["name"],"primitives":primitives}),
                );
                let mut renderer_node = json!({"name":format!("renderer_{renderer_id}"),"mesh":mesh,
                    "extras":{"renderer":renderer_id,"source_node":target,"skinned":renderer.kind==137}});
                if let Some(skin) = output_skin {
                    renderer_node["skin"] = skin.into();
                }
                let renderer_node = writer.add("nodes", renderer_node);
                if output_skin.is_some() {
                    writer.json["scenes"][0]["nodes"]
                        .as_array_mut()
                        .unwrap()
                        .push(renderer_node.into());
                } else {
                    writer.json["nodes"][target]
                        .as_object_mut()
                        .unwrap()
                        .entry("children")
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(renderer_node.into());
                }
                renderers
                    .entry(renderer_id)
                    .or_default()
                    .push(renderer_node);
                owners.entry(target).or_default().push(renderer_id);
            }
        }
        Ok(Assembly {
            writer,
            nodes,
            renderers,
            owners,
            animations,
            warnings,
            rig_count,
        })
    }
}
