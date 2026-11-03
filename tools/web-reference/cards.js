(function(){ var r={}; ['#tvfsUsersOnlyFeatureCard','#chunkConfigCard','#autoCopyCard'].forEach(function(s){var e=document.querySelector(s); r[s]=e? e.innerText.split('\n').map(function(x){return x.trim();}).filter(Boolean):null;});
 r.disc=[].map.call(document.querySelectorAll('.disclaimer-paragraphs p'),function(e){return e.innerText.replace(/\s+/g,' ').trim();});
 return r;})()
