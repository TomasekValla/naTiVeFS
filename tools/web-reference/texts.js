(function(){ function T(s){var e=document.querySelector(s); return e? e.innerText.replace(/\s+/g,' ').trim():null;}
 var r={}; ['#recentEmpty','#tvfsUsersOnlyFeatureCard','#chunkConfigCard','#autoCopyCard'].forEach(function(s){r[s]=T(s);});
 r.info=[].map.call(document.querySelectorAll('.info-card'),function(e){return e.innerText.replace(/\s+/g,' ').trim()+' | img='+(e.querySelector('img')||{}).src;});
 r.disc=[].map.call(document.querySelectorAll('.disclaimer-paragraphs p'),function(e){return e.innerText.replace(/\s+/g,' ').trim();});
 r.pills=[].map.call(document.querySelectorAll('.expiry-pill'),function(e){return e.innerText+(e.classList.contains('active')?'*':'');});
 return r;})()
