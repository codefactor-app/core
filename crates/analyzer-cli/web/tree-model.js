((root, factory) => {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.CodefactorTree = model;
})(typeof globalThis === 'object' ? globalThis : this, () => {
  'use strict';
  function fuzzyScore(candidate, query) {
    query=Array.from(String(query).toLowerCase()).filter(ch=>! /\s/u.test(ch)).join('');candidate=String(candidate||'').toLowerCase();
    if(!query||candidate===query)return 0;
    const q=Array.from(query),text=Array.from(candidate),start=candidate.indexOf(query);
    if(start>=0){const offset=Array.from(candidate.slice(0,start)).length;return (offset===0?20:60+offset*2)+Math.max(0,text.length-q.length);}
    let at=0,gap=0;for(let i=0;i<text.length;i++){if(text[i]===q[at]){at++;gap+=i-at+1;}if(at===q.length)return 120+gap+Math.max(0,text.length-q.length);}
    if(q.length<4||q.length>64)return null;
    const budget=q.length>=8?2:1;let best=null;
    for(const word of candidate.split(/[^\p{L}\p{N}]+/u)){const w=Array.from(word);if(Math.abs(w.length-q.length)>budget||w.length>64)continue;let older=[],previous=Array.from({length:w.length+1},(_,i)=>i);
      for(let i=0;i<q.length;i++){const current=[i+1];for(let j=0;j<w.length;j++){let cost=Math.min(previous[j+1]+1,current[j]+1,previous[j]+(q[i]!==w[j]?1:0));if(i>0&&j>0&&q[i]===w[j-1]&&q[i-1]===w[j])cost=Math.min(cost,older[j-1]+1);current.push(cost);}older=previous;previous=current;}
      const distance=previous[w.length];if(distance<=budget)best=Math.min(best??Infinity,300+distance*40);
    }return best;
  }
  function selector(node) {
    if(['directory','dir'].includes(node.kind)){const path=String(node.id).replace(/^\/+/,''),leaves=[];const walk=n=>n.children?.length?n.children.forEach(walk):leaves.push(n);walk(node);return `${leaves.length&&leaves.every(n=>n.vendor)?'vendor':'dir'}:${path}`;}
    if(node.kind==='file')return `${node.vendor?'vendor':'file'}:${node.path||node.id}`;
    const value=['class','struct','interface','trait','enum'].includes(node.kind)?`class:${node.fqn||node.name}`:node.symbol_id||node.id;
    return node.vendor?'vendor:'+value:value;
  }
  function prepare(nodes, parent = '') {
    return nodes.map(node => {
      const id = ['directory', 'dir'].includes(node.kind) ? `${parent}/${node.name}` : String(node.id);
      const children = prepare(node.children || [], id);
      const own = Number(node.violations || 0), nested = children.reduce((sum, child) => sum + child.violations, 0);
      return {...node, id, children, collapsed:children.length > 0,
        violations:node.kind === 'file' ? Math.max(own, nested) : own + nested};
    });
  }
  function visible(nodes, query = '', warningsOnly = false) {
    const needle = query.toLowerCase(), rows = [], matches = new Map();
    let files = 0;
    function match(node) {
      const children = node.children.map(match).some(Boolean);
      const own = !['directory', 'dir'].includes(node.kind)
        && (!needle || [node.path, node.fqn, node.name].some(value => value && fuzzyScore(value, needle)!==null))
        && (!warningsOnly || node.violations > 0);
      const found = own || children;
      matches.set(node.id, found);
      if (node.kind === 'file' && found) files++;
      return found;
    }
    nodes.forEach(match);
    function visit(node, depth) {
      if ((needle || warningsOnly) && !matches.get(node.id)) return;
      rows.push({node, depth});
      if (needle || warningsOnly || !node.collapsed) node.children.forEach(child => visit(child, depth + 1));
    }
    nodes.forEach(node => visit(node, 0));
    return {rows, files};
  }
  return {prepare, visible, fuzzyScore, selector};
});
