/* Graph visibility and layout are independent of DOM events and navigation history. */
((root, factory) => {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.CodefactorGraph = model;
})(typeof globalThis === 'object' ? globalThis : this, () => {
  'use strict';
  const WIDTH = 320, HEIGHT = 170;
  // Keep every character of namespaces and identifiers, including imported graphs.
  function wrapText(value, columns = 44) {
    const lines = [], chars = Array.from(String(value || ''));
    while (chars.length) {
      let end = 0, width = 0, separator = 0;
      while (end < chars.length) {
        const units = chars[end].codePointAt(0) >= 0x2e80 ? 2 : 1;
        if (width + units > columns) break;
        width += units;
        if (/^[\\/.: ]$/.test(chars[end])) separator = end + 1;
        end++;
      }
      if (end < chars.length && separator > 0) end = separator;
      lines.push(chars.splice(0, Math.max(1, end)).join(''));
    }
    return lines;
  }
  function nodeCard(node) {
    const label = String(node.label || node.id || '');
    const className = String(node.kind === 'class' ? label : node.parent_type || '');
    let functionName = node.kind === 'class' ? '' : node.behavior === 'module_initializer' ? 'Инициализация модуля' : String(node.name || label);
    if (!node.name && className && node.kind !== 'fold') {
      for (const separator of ['::', '.']) if (functionName.startsWith(className + separator)) {
        functionName = functionName.slice(className.length + separator.length);break;
      }
    }
    const classLines = wrapText(className), functionLines = wrapText(functionName, 36);
    const classBottom = 36 + Math.max(0, classLines.length - 1) * 14;
    const nameY = className ? classBottom + 40 : 38;
    const bottom = functionLines.length ? nameY + (functionLines.length - 1) * 18 : classBottom;
    const offset = Math.max(0, bottom + 33 - 82);
    return {className, functionName, classLines, functionLines, nameY, offset, height: HEIGHT + offset};
  }
  function projectData(data) {
    const excluded=new Set(data.nodes.filter(n=>n.category==='runtime'||n.standard===true).map(n=>String(n.id)));
    const keep=edge=>!excluded.has(String(edge.from))&&!excluded.has(String(edge.to));
    return {...data,nodes:data.nodes.filter(n=>!excluded.has(String(n.id))),edges:data.edges.filter(keep),cycles:(data.cycles||[]).filter(keep),hidden_vendor_edges:(data.hidden_vendor_edges||[]).filter(keep)};
  }
  function externalCallsites(data, id) {
    return [...(data.edges || []), ...(data.cycles || [])].filter(edge => String(edge.to) === String(id));
  }
  class View {
    constructor(data, direction = 'both') {
      data=projectData(data); this.data = data; this.root = String(data.root || data.roots?.[0]); this.direction = direction;
      this.byId = new Map(data.nodes.map(n => [String(n.id), n]));
      this.out = new Map(); this.into = new Map();
      for (const edge of data.edges) {
        const from = String(edge.from), to = String(edge.to);
        if (!this.byId.has(from) || !this.byId.has(to)) continue;
        if (!this.out.has(from)) this.out.set(from, new Set());
        if (!this.into.has(to)) this.into.set(to, new Set());
        this.out.get(from).add(to); this.into.get(to).add(from);
      }
      this.sides = new Map();
      this.levels = new Map([[this.root, 0]]); this.distance = new Map([[this.root, 0]]);
      for (const sign of this.signs()) {
        const map = sign < 0 ? this.into : this.out, seen = new Set([this.root]), queue = [[this.root, 0]];
        for (let at = 0; at < queue.length; at++) {
          const [id, depth] = queue[at];
          for (const next of map.get(id) || []) {
            if (seen.has(next)) continue;
            seen.add(next); if(!this.sides.has(next))this.sides.set(next,new Set());this.sides.get(next).add(sign); const d = depth + 1;
            if (!this.distance.has(next) || d < this.distance.get(next)) { this.distance.set(next, d); this.levels.set(next, sign * d); }
            queue.push([next, d]);
          }
        }
      }
      this.revealed = new Set([this.root]); this.reachCache = new Map();
      this.neighborGroups=new Map(); this.neighborShown=new Map();
      for (const sign of this.signs()) {
        const ids=[...(this.adjacency(sign).get(this.root)||[])].filter(id=>id!==this.root).sort();
        if(ids.length>8){const key=`neighbors:${sign}:${this.root}`;this.neighborGroups.set(key,{sign,ids});this.neighborShown.set(key,0);}
        else for(const id of ids)this.revealed.add(id);
      }
      this.baseInitial = new Set(this.revealed); this.initial = new Set(this.revealed); this.expansions = new Map();
      this.groupExpanded = new Set();
    }
    signs() { return this.direction === 'callers' ? [-1] : this.direction === 'callees' ? [1] : [-1, 1]; }
    adjacency(sign) { return sign < 0 ? this.into : this.out; }
    nodeSigns(id) { return id === this.root ? this.signs() : [...(this.sides.get(id)||new Set([(this.levels.get(id)||1)<0?-1:1]))]; }
    reachable(id, sign) {
      const key = `${sign}:${id}`;
      if (this.reachCache.has(key)) return this.reachCache.get(key);
      const seen = new Set([id]), list = [], queue = [id], map = this.adjacency(sign);
      for (let at = 0; at < queue.length; at++) for (const next of map.get(queue[at]) || []) {
        if (seen.has(next) || next === this.root && id !== this.root) continue; seen.add(next); list.push(next); queue.push(next);
      }
      this.reachCache.set(key, list); return list;
    }
    foldedNeighbor(id, next, sign) {
      if(id!==this.root||this.revealed.has(next))return false;
      return this.neighborGroups.has(`neighbors:${sign}:${this.root}`);
    }
    hidden(id) {
      if (this.revealed.size === this.byId.size) return { nodes: 0, branches: 0 };
      const hidden = new Set(), frontier = new Set();
      for (const sign of this.nodeSigns(id)) {
        const map = this.adjacency(sign);
        for (const next of this.reachable(id, sign)) if (!this.revealed.has(next)) hidden.add(next);
        const seen = new Set([id]), queue = [id];
        for (let at = 0; at < queue.length; at++) for (const next of map.get(queue[at]) || []) {
          if (seen.has(next) || next === this.root && id !== this.root) continue; seen.add(next);
          if (!this.revealed.has(next)) frontier.add(next); else queue.push(next);
        }
      }
      return { nodes: hidden.size, branches: frontier.size };
    }
    expand(id, all = false) {
      const added = this.expansions.get(id) || new Set();
      for (const sign of this.nodeSigns(id)) {
        if (all) {
          if(id===this.root)for(const [key,group] of this.neighborGroups)if(group.sign===sign)this.neighborShown.set(key,group.ids.length);
          for (const next of this.reachable(id, sign)) added.add(next); continue;
        }
        // Visit through already visible nodes and add only the next hidden frontier.
        const map = this.adjacency(sign), seen = new Set([id]), queue = [id], frontier = new Set();
        for (let at = 0; at < queue.length; at++) for (const next of map.get(queue[at]) || []) {
          if (seen.has(next) || next === this.root && id !== this.root) continue; seen.add(next);
          if(this.foldedNeighbor(id,next,sign))continue;
          if (!this.revealed.has(next)) frontier.add(next); else { queue.push(next); added.add(next); }
        }
        for (const next of frontier) added.add(next);
      }
      if (added.size) this.expansions.set(id, added);
      this.refreshVisibility();
    }
    expandNeighbors(key, all=false) {
      const group=this.neighborGroups.get(key);if(!group)return;
      const visible=group.ids.filter(id=>this.revealed.has(id)).length;
      this.neighborShown.set(key,all?group.ids.length:Math.min(group.ids.length,visible+20));
      this.refreshVisibility();
    }
    closeNeighbors(key) {
      const group=this.neighborGroups.get(key);if(!group)return;
      const exclusive=id=>this.sides.get(id)?.has(group.sign)&&!this.sides.get(id)?.has(-group.sign);
      for(const anchor of this.expansions.keys())if(exclusive(anchor))this.expansions.delete(anchor);
      const rootExpansion=this.expansions.get(this.root);
      if(rootExpansion){for(const id of rootExpansion)if(exclusive(id))rootExpansion.delete(id);if(!rootExpansion.size)this.expansions.delete(this.root);}
      this.neighborShown.set(key,0);this.refreshVisibility();
      for(const anchor of this.expansions.keys())if(!this.revealed.has(anchor))this.expansions.delete(anchor);
    }
    refreshVisibility() {
      this.initial = new Set(this.baseInitial);
      for(const [key,group] of this.neighborGroups)for(const id of group.ids.slice(0,this.neighborShown.get(key)||0))this.initial.add(id);
      this.revealed = new Set(this.initial);
      let changed = true;
      while (changed) {
        changed = false;
        for (const [anchor, added] of this.expansions) if (this.revealed.has(anchor)) {
          for (const id of added) if (!this.revealed.has(id)) { this.revealed.add(id); changed = true; }
        }
      }
    }
    close(id) {
      if (id === this.root) { this.collapse(); return; }
      this.expansions.delete(id); this.refreshVisibility();
      // Shared nodes stay visible through another expanded branch. Inactive expansions
      // are forgotten so reopening an isolated branch starts one level at a time.
      for (const anchor of this.expansions.keys()) if (!this.revealed.has(anchor)) this.expansions.delete(anchor);
    }
    expanded(id) { return this.expansions.has(id); }
    collapse() {
      this.neighborShown=new Map([...this.neighborGroups.keys()].map(key=>[key,0])); this.expansions.clear(); this.groupExpanded.clear(); this.refreshVisibility();
    }
    update(data) {
      const refreshed = new View(data, this.direction);
      refreshed.expansions = this.expansions;
      refreshed.groupExpanded = this.groupExpanded;
      for (const [key, count] of this.neighborShown) if (refreshed.neighborGroups.has(key)) refreshed.neighborShown.set(key, count);
      Object.assign(this, refreshed);
      this.refreshVisibility();
    }
    layout({ collapseSimple = true, groupClasses = true, positions = new Map(), limit = 600 } = {}) {
      const candidates = [...this.revealed].filter(id => this.byId.has(id)).sort((a, b) => (this.distance.get(a) || 0) - (this.distance.get(b) || 0) || a.localeCompare(b));
      const shown = new Set(candidates), groups = new Map(), mapping = new Map();
      let nodes = [];
      for (const id of shown) {
        const n = this.byId.get(id), level = this.levels.get(id) || 0;
        const category = ['getter', 'setter'].includes(n.behavior) ? 'accessors' : n.behavior === 'simple_leaf' ? 'simple' : null;
        // A foldable group is made of terminal nodes in this traversal direction.
        const hasBranches = this.nodeSigns(id).some(sign => (this.adjacency(sign).get(id)?.size || 0) > 0);
        const key = `fold:${level}:${n.language || ''}:${n.file || ''}:${n.parent_type || ''}:${category}`;
        if (id !== this.root && collapseSimple && category && !hasBranches) {
          if (!groups.has(key)) groups.set(key, { id: key, kind: 'fold', file: n.file, parent_type: n.parent_type, language: n.language, level, category, members: [], label: category === 'accessors' ? 'Геттеры / сеттеры' : 'Короткие без вызовов' });
          groups.get(key).members.push(n);
        } else { nodes.push({ ...n, level }); mapping.set(id, id); }
      }
      for (const group of groups.values()) {
        if (this.groupExpanded.has(group.id)) for (const member of group.members) { nodes.push({ ...member, level: group.level }); mapping.set(String(member.id), String(member.id)); }
        else { nodes.push(group); for (const member of group.members) mapping.set(String(member.id), group.id); }
      }
      for(const [id,group] of this.neighborGroups) {
        const members=group.ids.filter(peer=>!this.revealed.has(peer)).map(peer=>this.byId.get(peer));
        if(!members.length)continue;
        const fold={id,kind:'fold',level:group.sign,category:group.sign<0?'callers':'callees',label:group.sign<0?'Вызывающие методы':'Вызываемые методы',members,total:group.ids.length};
        nodes.push(fold);groups.set(id,fold);for(const member of members)mapping.set(String(member.id),id);
      }
      const unitCount = nodes.length;
      nodes.sort((a, b) => Math.abs(a.level) - Math.abs(b.level) || String(a.id).localeCompare(String(b.id)));
      nodes = nodes.slice(0, limit);
      const heights = new Map(nodes.map(n => [String(n.id), nodeCard(n).height]));
      const units = new Set(nodes.map(n => String(n.id)));
      for (const [id, target] of mapping) if (!units.has(target)) mapping.delete(id);
      const buckets = new Map();
      for (const n of nodes) { if (!buckets.has(n.level)) buckets.set(n.level, []); buckets.get(n.level).push(n); }
      const spacing = 520 + Math.min(240, Math.sqrt(Math.max(1, ...[...buckets.values()].map(b => b.length))) * 22);
      const placed = new Map(), frames = [];
      for (const [level, bucket] of buckets) {
        const sections = new Map();
        for (const n of bucket) {
          const key = groupClasses && n.file ? `file:${n.language || ''}:${n.file}:${level}` : `single:${n.id}`;
          if (!sections.has(key)) sections.set(key, { key, file: n.file, nodes: [] }); sections.get(key).nodes.push(n);
        }
        const list = [...sections.values()].sort((a, b) => a.key.localeCompare(b.key));
        for (const section of list) {
          const functions = section.nodes.filter(n => ['function','method','fold'].includes(n.kind));
          section.framed = groupClasses && section.file && functions.length > 1;
          const owners = new Set(functions.map(n => n.parent_type));
          section.owner = owners.size === 1 ? [...owners][0] : null;
          section.headerLines = wrapText(section.owner || section.file);
          section.headerHeight = Math.max(48, 42 + (section.headerLines.length - 1) * 14);
        }
        const height = section => section.nodes.reduce((sum, n) => sum + heights.get(String(n.id)) + 42, 0) + (section.framed ? section.headerHeight + 6 : 0) + 32;
        let y = -list.reduce((total, s) => total + height(s), 0) / 2;
        for (const section of list) {
          const ids = [];
          if (section.framed) y += section.headerHeight + 6;
          for (const n of section.nodes) { placed.set(n.id, positions.get(n.id) || { x: level * spacing, y }); ids.push(n.id); y += heights.get(String(n.id)) + 42; }
          if (section.framed) frames.push({ id: section.key, label: section.owner || section.file, headerLines:section.headerLines, headerHeight:section.headerHeight, owner: section.owner, file: section.file, level, members: ids, memberHeights:Object.fromEntries(ids.map(id=>[id,heights.get(String(id))])), language: section.nodes[0]?.language });
          y += 32;
        }
      }
      if (shown.has(this.root) && !positions.has(this.root)) placed.set(this.root, { x: 0, y: 0 });
      // Parallel calls are bundled visually, with every callsite/condition available in details.
      const bundles = new Map();
      for (const edge of this.data.edges) {
        const from = mapping.get(String(edge.from)), to = mapping.get(String(edge.to));
        if (!from || !to || from === to && String(edge.from) !== String(edge.to)) continue;
        const key = `${from}\u0000${to}`;
        if (!bundles.has(key)) bundles.set(key, { from, to, calls: [] }); bundles.get(key).calls.push(edge);
      }
      return { root: this.root, nodes, neighborGroups:this.neighborGroups, positions: placed, heights, frames, bundles: [...bundles.values()], groups, remaining: unitCount - nodes.length, revealed: candidates.length, width: WIDTH, height: HEIGHT };
    }
  }
  function frameBounds(frame, positions) {
    const points = frame.members.map(id => ({...positions.get(id),height:frame.memberHeights?.[id] || HEIGHT})).filter(p=>p.x!=null);
    const x = Math.min(...points.map(p => p.x)) - 22, y = Math.min(...points.map(p => p.y)) - (frame.headerHeight || 48);
    return { x, y, width: Math.max(...points.map(p => p.x)) + WIDTH + 22 - x, height: Math.max(...points.map(p => p.y + p.height)) + 22 - y };
  }
  function hiddenVendorIndex(data) {
    const index = new Map();
    const nodes = new Map([...(data.nodes || []), ...(data.hidden_vendor_nodes || [])].map(node => [String(node.id), node]));
    for (const edge of data.hidden_vendor_edges || []) {
      for (const id of [String(edge.from), String(edge.to)]) {
        if (!nodes.has(id) || nodes.get(id).vendor) continue;
        if (!index.has(id)) index.set(id, {edges:[], packages:new Set()});
        const item = index.get(id);item.edges.push(edge);
        for (const other of [String(edge.from), String(edge.to)]) {
          const vendor = nodes.get(other);
          if (vendor?.vendor) item.packages.add(vendor.package || vendor.label || other);
        }
      }
    }
    return index;
  }
  function chainRows(data, direction = 'callees', limit = 600) {
    const nodes = new Map(data.nodes.map(node => [String(node.id), node])), map = new Map();
    const reverse = direction === 'callers';
    for (const edge of data.edges.filter(edge => edge.relation !== 'member')) {
      const id = String(reverse ? edge.to : edge.from);
      if (!map.has(id)) map.set(id, []);
      map.get(id).push(edge);
    }
    const root = String(data.root || data.roots?.[0]), rows = [], classRoot = nodes.get(root)?.kind === 'class';
    const starts = classRoot ? (data.roots || []).map(String) : [root];
    const stack = starts.slice().reverse().map(id => ({id, depth:0, path:[], guards:[]}));
    let truncated = false;
    while (stack.length) {
      if (rows.length >= limit) {truncated = true;break;}
      const current = stack.pop(), cycle = current.path.includes(current.id), node = nodes.get(current.id);
      if (!node) continue;
      rows.push({...current, node, cycle});
      if (cycle) continue;
      const edges = map.get(current.id) || [];
      for (let i = edges.length - 1; i >= 0; i--) {
        const edge = edges[i], guards = [...current.guards, ...(edge.guard || edge.guards || []).filter(g => String(g.normalized || g.raw || '').trim())];
        stack.push({id:String(reverse ? edge.from : edge.to), depth:current.depth + 1,
          path:[...current.path, current.id], guards, edge});
      }
    }
    return {rows, truncated};
  }
  const edgeIdentity = edge => edge.load_key != null ? String(edge.load_key) : JSON.stringify(edge);
  class LazyGraph {
    constructor(data, {direction = 'both', depth = null, vendor = 'collapsed'} = {}) {
      this.root = String(data.root || data.roots?.[0]);
      this.roots = (data.roots || [this.root]).map(String);
      this.direction = direction;this.depth = depth ? Number(depth) : null;this.vendor = vendor;
      this.revision = data.revision;
      this.requestedMetadata = Object.fromEntries(['requested_coverage','coverage_complete','coverage_unparsed_vendor_files','coverage_unparsed_vendor_packages','coverage_direction','coverage_depth','coverage_revision','coverage_filters','coverage_vendor_view'].filter(key=>data[key]!==undefined).map(key=>[key,data[key]]));
      this.rawNodes = new Map();this.rawEdges = new Map();this.rawCycles = new Map();this.frontier = new Map();
      this.distanceCache = new Map();
      this.displayNodes = new Map();this.synthetic = (data.edges || []).filter(edge => edge.relation === 'member');
      this.add(data, {id:this.root, dir:direction, depth:1, initial:true});
      this.data = this.project();
    }
    signs() {return this.direction === 'both' ? [-1, 1] : [this.direction === 'callers' ? -1 : 1];}
    adjacency(edges, sign) {
      const map = new Map();
      for (const edge of edges) {
        const source = String(sign < 0 ? edge.to : edge.from);
        if (!map.has(source)) map.set(source, []);
        map.get(source).push(edge);
      }
      return map;
    }
    add(response, request) {
      this.distanceCache.clear();
      const nodes = response.coverage_nodes || response.nodes || [], selected = response.coverage_edges || response.edges || [], cycles = response.coverage_cycles || response.cycles || [];
      const all = [...selected, ...cycles], keepNodes = new Set(), keepEdges = new Set();
      const signs = request.dir === 'both' ? [-1, 1] : [request.dir === 'callers' ? -1 : 1];
      for (const sign of signs) {
        const map = this.adjacency(all, sign), starts = request.initial ? this.roots : [request.id], seen = new Set(starts), queue = starts.map(id => [id, 0]);
        starts.forEach(id => keepNodes.add(id));
        for (let at = 0; at < queue.length; at++) {
          const [id, depth] = queue[at];
          if (request.depth != null && depth >= request.depth || !request.initial && id === this.root && id !== request.id) continue;
          for (const edge of map.get(id) || []) {
            const next = String(sign < 0 ? edge.from : edge.to);
            keepEdges.add(edgeIdentity(edge));keepNodes.add(next);
            if (!seen.has(next)) {seen.add(next);queue.push([next, depth + 1]);}
          }
        }
      }
      for (const node of nodes) if (keepNodes.has(String(node.id))) this.rawNodes.set(String(node.id), node);
      for (const edge of selected) if (keepEdges.has(edgeIdentity(edge))) this.rawEdges.set(edgeIdentity(edge), edge);
      for (const edge of cycles) if (keepEdges.has(edgeIdentity(edge))) this.rawCycles.set(edgeIdentity(edge), edge);
      for (const [id, hints] of Object.entries(response.frontier || {})) if (keepNodes.has(id)) this.frontier.set(id, hints);
      for (const node of response.nodes || []) this.displayNodes.set(String(node.id), node);
    }
    distances(sign) {
      if (this.distanceCache.has(sign)) return this.distanceCache.get(sign);
      const map = this.adjacency([...this.rawEdges.values(), ...this.rawCycles.values()], sign), result = new Map(this.roots.map(id => [id, 0])), queue = [...this.roots];
      for (let at = 0; at < queue.length; at++) for (const edge of map.get(queue[at]) || []) {
        const next = String(sign < 0 ? edge.from : edge.to);
        if (!result.has(next)) {result.set(next, result.get(queue[at]) + 1);queue.push(next);}
      }
      this.distanceCache.set(sign, result);return result;
    }
    remaining(id, sign) {
      const distance = this.distances(sign).get(String(id));
      return distance == null ? 0 : this.depth == null ? Infinity : Math.max(0, this.depth - distance);
    }
    pending(id, sign) {
      if (!this.remaining(id, sign)) return [];
      const hints = this.frontier.get(String(id))?.[sign < 0 ? 'callers' : 'callees'] || [];
      return [...new Map(hints.filter(hint => !this.rawEdges.has(String(hint.key)) && !this.rawCycles.has(String(hint.key))).map(hint => [String(hint.key), hint])).values()];
    }
    frontierSources(view, id, sign, all = false) {
      const result = [], seen = new Set([id]), queue = [id], map = view.adjacency(sign);
      for (let at = 0; at < queue.length; at++) {
        const current = queue[at];
        if (current === view.root && current !== id) continue;
        if (this.pending(current, sign).length) result.push(current);
        for (const next of map.get(current) || []) {
          if (seen.has(next) || !all && (!view.revealed.has(next) || view.foldedNeighbor(id, next, sign))) continue;
          seen.add(next);queue.push(next);
        }
      }
      return result;
    }
    plan(view, id, all = false) {
      const requests = new Map();
      for (const sign of view.nodeSigns(id)) {
        const sources = this.frontierSources(view, id, sign, all);
        if (!sources.length) continue;
        const anchors = all ? id === this.root && !this.rawNodes.has(id) ? this.roots : [id] : sources;
        for (const anchor of anchors) {
          const remaining = this.remaining(anchor, sign);
          if (!remaining) continue;
          const dir = sign < 0 ? 'callers' : 'callees', depth = all ? remaining === Infinity ? null : remaining : 1;
          requests.set(`${anchor}:${dir}`, {id:anchor, dir, depth});
        }
      }
      return [...requests.values()];
    }
    remote(view, id) {
      return view.nodeSigns(id).some(sign => this.frontierSources(view, id, sign).length > 0);
    }
    async expand(view, id, all, fetchBranch, isCurrent = () => true) {
      const plan = this.plan(view, id, all);
      const responses = await Promise.all(plan.map(fetchBranch));
      if (!isCurrent()) return {stale:true};
      if (responses.some(response => response.revision !== this.revision)) return {revisionChanged:true};
      for (let index = 0; index < responses.length; index++) this.add(responses[index], plan[index]);
      this.data = this.project();view.update(this.data);view.expand(id, all);
      return {data:this.data, requests:plan.length};
    }
    project() {
      const all = new Map([...this.rawCycles, ...this.rawEdges]), distances = this.signs().map(sign => [sign, this.distances(sign)]), ids = new Set(this.roots), evidence = new Map();
      for (const [sign, distance] of distances) {
        for (const [id, depth] of distance) if (this.depth == null || depth <= this.depth) ids.add(id);
        for (const [key, edge] of all) {
          const source = String(sign < 0 ? edge.to : edge.from), depth = distance.get(source);
          if (depth != null && (this.depth == null || depth < this.depth)) evidence.set(key, edge);
        }
      }
      const rawNodes = [...ids].map(id => this.rawNodes.get(id)).filter(Boolean), coverage = {
        functions:[],classes:[],files:[],modules:[],vendor_functions:[],vendor_classes:[],external:[],unresolved:[],entry_points:[],leaves:[],vendor_boundary:rawNodes.some(node=>node.vendor&&this.vendor!=='expanded'),
      };
      const incoming = new Set([...evidence.values()].map(edge => String(edge.to))), outgoing = new Set([...evidence.values()].map(edge => String(edge.from)));
      for (const node of rawNodes) {
        if (['function','method'].includes(node.kind)) {
          coverage[node.vendor ? 'vendor_functions' : 'functions'].push(node.label);
          if (node.parent_type) coverage[node.vendor ? 'vendor_classes' : 'classes'].push(node.parent_type);
          if (!node.vendor) {if(node.file)coverage.files.push(node.file);if(node.module)coverage.modules.push(node.module);}
          if (!incoming.has(String(node.id))) coverage.entry_points.push(node.label);
          if (!outgoing.has(String(node.id))) coverage.leaves.push(node.label);
        } else if (node.kind === 'external') coverage.external.push(node.label);
        else if (node.kind === 'unknown') coverage.unresolved.push(node.label);
        else if (node.kind === 'vendor_package') coverage.vendor_boundary = true;
      }
      for (const key of Object.keys(coverage)) if (Array.isArray(coverage[key])) coverage[key] = [...new Set(coverage[key])].sort();
      const mapping = new Map(), nodes = new Map(), hiddenNodes = [];
      for (const node of rawNodes) {
        if (node.vendor && this.vendor !== 'expanded') {
          hiddenNodes.push(node);
          if (this.vendor === 'hidden') continue;
          const id = `vendor:${node.package || node.label}`;mapping.set(String(node.id), id);
          nodes.set(id, this.displayNodes.get(id) || {id,label:node.package || node.label,kind:'vendor_package',vendor:true,package:node.package || node.label});
        } else {mapping.set(String(node.id), String(node.id));nodes.set(String(node.id), this.displayNodes.get(String(node.id)) || node);}
      }
      const edges = [], cycles = [], hidden = [];
      for (const [key, edge] of evidence) {
        const from = mapping.get(String(edge.from)), to = mapping.get(String(edge.to));
        if (!from || !to) {if(this.vendor==='hidden')hidden.push({...edge,hidden_vendor:true});continue;}
        const projected = {...edge,from,to};edges.push(projected);
        if (this.rawCycles.has(key)) cycles.push(projected);
      }
      if (!nodes.has(this.root) && this.displayNodes.has(this.root)) nodes.set(this.root, this.displayNodes.get(this.root));
      for (const edge of this.synthetic) {
        const from = mapping.get(String(edge.from)) || String(edge.from), to = mapping.get(String(edge.to)) || String(edge.to);
        if (nodes.has(from) && nodes.has(to)) edges.push({...edge,from,to});
      }
      const unloaded = rawNodes.map(node => ({id:String(node.id),directions:this.signs().filter(sign=>this.pending(String(node.id),sign).length).map(sign=>sign<0?'callers':'callees')})).filter(item=>item.directions.length);
      const partial = unloaded.length > 0;
      let loadedDepth = 0;
      for (const [, distance] of distances) for (const [id, depth] of distance) if(ids.has(id)&&(this.depth==null||depth<=this.depth))loadedDepth=Math.max(loadedDepth,depth);
      return {root:this.root,roots:this.roots,nodes:[...nodes.values()],edges,cycles,hidden_vendor_edges:hidden,hidden_vendor_nodes:hiddenNodes,
        ...this.requestedMetadata,coverage:this.requestedMetadata.requested_coverage||coverage,loaded_coverage:coverage,coverage_scope:this.requestedMetadata.requested_coverage?'requested':'loaded',coverage_nodes:rawNodes,coverage_edges:[...evidence.values()],frontier:Object.fromEntries(this.frontier),
        direction:this.direction,vendor_view:this.vendor,depth:null,requested_depth:this.depth,loaded_depth:loadedDepth,lazy:true,partial,unloaded_frontiers:unloaded,revision:this.revision};
    }
  }
  function exportMetadata(data) {
    const keys=['revision','direction','vendor_view','depth','requested_depth','loaded_depth','lazy','partial','coverage_scope','coverage_complete','coverage_unparsed_vendor_files','coverage_unparsed_vendor_packages','coverage_direction','coverage_depth','coverage_revision','coverage_filters','coverage_vendor_view','requested_coverage','loaded_coverage'];
    return {...Object.fromEntries(keys.filter(key=>data[key]!==undefined).map(key=>[key,data[key]])),coverage:data.coverage,
      loaded_nodes:(data.coverage_nodes||data.nodes||[]).length,loaded_edges:(data.coverage_edges||data.edges||[]).length};
  }
  async function pngWithMetadata(blob, metadata) {
    const bytes=new Uint8Array(await blob.arrayBuffer()),text=new TextEncoder().encode(JSON.stringify(metadata));
    // PNG iTXt: keyword, uncompressed text flag/method, empty language and
    // translated keyword, then UTF-8 metadata. Insert before the final IEND.
    const prefix=new Uint8Array([67,111,100,101,102,97,99,116,111,114,0,0,0,0,0]);
    const body=new Uint8Array(prefix.length+text.length);body.set(prefix);body.set(text,prefix.length);
    const chunk=new Uint8Array(body.length+12),view=new DataView(chunk.buffer);view.setUint32(0,body.length);
    chunk.set([105,84,88,116],4);chunk.set(body,8);
    let crc=0xffffffff;for(const byte of chunk.subarray(4,chunk.length-4)){crc^=byte;for(let bit=0;bit<8;bit++)crc=(crc>>>1)^((crc&1)?0xedb88320:0);}
    view.setUint32(chunk.length-4,(crc^0xffffffff)>>>0);
    return new Blob([bytes.subarray(0,bytes.length-12),chunk,bytes.subarray(bytes.length-12)],{type:'image/png'});
  }
  return { View, LazyGraph, projectData, externalCallsites, frameBounds, hiddenVendorIndex, chainRows, exportMetadata, pngWithMetadata, nodeCard, wrapText, WIDTH, HEIGHT };
});
