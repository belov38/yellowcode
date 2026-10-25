import { api } from "./api.js";

const metadataSections = new Map();

export function metadataSection(kind) {
  if (metadataSections.has(kind)) return metadataSections.get(kind);
  const request = (async () => {
    const roots = await api("tree");
    const common = roots.find((n) => n.icon === "group:Common");
    let section = roots.find((n) => n.icon === "section:" + kind);
    if (!section && common) section = (await api("tree", { id: common.id })).find((n) => n.icon === "section:" + kind);
    if (!section) return [];
    const seen = new Set();
    const walk = async (id, depth) => {
      if (seen.has(id)) return [];
      seen.add(id);
      const nodes = await api("tree", { id });
      const result = [];
      for (const node of nodes) {
        if (node.path) {
          result.push({ ...node, depth });
          if (kind === "Subsystem" && node.children) result.push(...await walk(node.id, depth + 1));
        } else if (kind === "Subsystem" && ["section:Subsystem", "folder:Subsystems"].includes(node.icon)) {
          result.push(...await walk(node.id, depth));
        }
      }
      return result;
    };
    return walk(section.id, 0);
  })();
  metadataSections.set(kind, request);
  request.catch(() => metadataSections.delete(kind));
  return request;
}


const reverseIndexes = new Map();

export function reverseReferences(property, target) {
  if (!reverseIndexes.has(property)) {
    const request = (async () => {
      const kinds = ["Catalog", "Document", "ChartOfAccounts", "ChartOfCalculationTypes", "ChartOfCharacteristicTypes", "ExchangePlan", "BusinessProcess", "Task"];
      const nodes = (await Promise.all(kinds.map(metadataSection))).flat();
      const index = new Map();
      for (let offset = 0; offset < nodes.length; offset += 8) {
        const batch = nodes.slice(offset, offset + 8);
        const values = await Promise.all(batch.map((node) => api("show", { path: node.path })));
        values.forEach((value, i) => {
          const refs = value.properties.find((p) => p.name === property)?.value?.refs || [];
          for (const ref of refs) {
            if (!index.has(ref)) index.set(ref, []);
            index.get(ref).push(batch[i]);
          }
        });
      }
      return index;
    })();
    reverseIndexes.set(property, request);
    request.catch(() => reverseIndexes.delete(property));
  }
  return reverseIndexes.get(property).then((index) => index.get(target) || []);
}

let recordableInformationRegisters;

export function informationRegistersForPosting() {
  if (!recordableInformationRegisters) {
    recordableInformationRegisters = (async () => {
      const nodes = await metadataSection("InformationRegister");
      const result = [];
      for (let offset = 0; offset < nodes.length; offset += 8) {
        const batch = nodes.slice(offset, offset + 8);
        const values = await Promise.all(batch.map((node) => api("show", { path: node.path })));
        batch.forEach((node, i) => {
          if (values[i].properties.some((p) => p.name === "WriteMode" && p.value === "RecorderSubordinate")) result.push(node);
        });
      }
      return result;
    })();
    recordableInformationRegisters.catch(() => { recordableInformationRegisters = null; });
  }
  return recordableInformationRegisters;
}
