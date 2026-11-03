(function(){
  function sel(e){ var s=e.tagName.toLowerCase(); if(e.id) s+='#'+e.id; if(e.className && typeof e.className==='string') s+='.'+e.className.trim().split(/\s+/).slice(0,3).join('.'); return s; }
  var out=[]; var root=document.body;
  function walk(e, d){
    if(d>7) return;
    var cs=getComputedStyle(e), r=e.getBoundingClientRect();
    if(cs.display==='none' || r.width===0 || r.height===0) return;
    var own = (e.childNodes.length && Array.prototype.some.call(e.childNodes,function(n){return n.nodeType===3 && n.textContent.trim();})) ? e.textContent.trim().slice(0,28) : '';
    var bg=cs.backgroundColor, bgi=cs.backgroundImage!=='none'?cs.backgroundImage.slice(0,110):'';
    var bw=cs.borderTopWidth, bc=cs.borderTopColor;
    out.push({d:d, s:sel(e), x:Math.round(r.x), y:Math.round(r.y+window.scrollY), w:Math.round(r.width), h:Math.round(r.height),
      bg:bg==='rgba(0, 0, 0, 0)'?'':bg, bgi:bgi, br:cs.borderRadius, bd:(bw!=='0px'?bw+' '+bc+' '+cs.borderTopStyle:''),
      pad:cs.padding, fs:cs.fontSize, fw:cs.fontWeight, ff:cs.fontFamily.slice(0,40), col:cs.color, ls:cs.letterSpacing, tt:cs.textTransform,
      sh:cs.boxShadow!=='none'?cs.boxShadow.slice(0,90):'', bf:(cs.backdropFilter&&cs.backdropFilter!=='none')?cs.backdropFilter:'', txt:own});
    for(var i=0;i<e.children.length;i++) walk(e.children[i], d+1);
  }
  walk(root,0);
  return {w:innerWidth, h:document.documentElement.scrollHeight, items:out};
})()
