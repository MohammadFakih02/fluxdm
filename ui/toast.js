// The page owns the visible notification and preserves arrival order and hover holds.
"use strict";
let reveal=null, hideAt=0, paused=false, shownAt=0, current=null, settings={}, frozenLeft=0;
const queue=[], countdowns=new Map();
const SHOW_MS=6500, MIN_MS=2500, MAX_HOVER_MS=15000;
$("#x").innerHTML=icon("x");
invoke("get_settings").then(g=>{settings=g.settings;applyAppearance(settings);applySurfaceArt(current?.banner,settings);}).catch(()=>{});
const same=(a,b)=>a&&b&&a.title===b.title&&a.body===b.body&&a.id===b.id&&a.token===b.token;
function receive(p) {
  if(same(queue[queue.length-1]||current,p)) return;
  if(current) { queue.push(p); if(current.kind!=="countdown") hideAt=Math.min(hideAt,Math.max(shownAt+MIN_MS,Date.now()+1500)); }
  else show(p);
}
listen("toast",receive).then(()=>invoke("toast_ready")).then(waiting=>(waiting||[]).forEach(receive));
listen("toast-reset",()=>{current=null;hideAt=0;next();});
listen("countdown-state",p=>{
  countdowns.set(p.token,p);
  if(countdowns.size>100) countdowns.delete(countdowns.keys().next().value);
  if(current?.token===p.token) {
    if(p.closed) finish(); else renderCountdown(p);
  }
});
function button(text,command,args,primary=false) {
  return el("button",{text,class:primary?"primary":"",onclick:async e=>{
    e.stopPropagation();
    const gen=current?.gen;
    try { await invoke(command,args); if(current?.gen===gen) finish(); }
    catch(err) { if(current?.gen===gen) $("#body").textContent=String(err); }
  }});
}
function renderCountdown(p) {
  $("#title").textContent=p.queue?.trim() ? p.queue.trim()+" queue finished" : "Queue finished";
  const words={Exit:"FluxDM will exit",Sleep:"The PC will go to sleep",Hibernate:"The PC will hibernate",Shutdown:"The PC will shut down",Restart:"The PC will restart"};
  $("#body").replaceChildren((words[p.action]||p.action)+" in ",el("span",{class:"mono",text:p.seconds+" s"}));
  $("#countdownBar").style.width=Math.max(0,p.seconds/60*100)+"%";
}
function show(p) {
  if(p.kind==="countdown" && countdowns.get(p.token)?.closed) { next(); return; }
  current=p; paused=false; shownAt=Date.now(); hideAt=shownAt+SHOW_MS;
  reveal=p.reveal||null;
  const countdown=p.kind==="countdown", error=["error","retrying"].includes(p.kind), warning=p.kind==="warning";
  $("#t").className=countdown?"countdown":error?"error":(p.game ?? !!p.banner)?"game":p.torrent?"torrent":"";
  $("#title").textContent=p.title||"FluxDM";
  $("#body").textContent=p.kind==="finished" ? [p.fileName,p.size,p.checksum?"checksum OK":"",p.duration?"took "+p.duration:""].filter(Boolean).join(" · ") : p.body||"";
  const notice=!p.kind && !p.id; // app messages ("still running", updates…) aren't successes: no green check
  $("#t").classList.toggle("notice",notice);
  $("#t").classList.toggle("warning",warning);
  // Only a retry is a connection problem; other failures (disk full, 404…) and warnings get the warning sign.
  $("#logo").innerHTML=countdown||error||warning ? icon(countdown?"clock":p.kind==="retrying"?"wifiOff":"warn") : notice ? '<img alt="" class="app-logo" src="'+(typeof appLogo==="function"?appLogo():"icon.png")+'">' : p.icon?'<img alt="" src="'+esc(p.icon)+'">':icon(p.torrent?"magnet":"check");
  setCardArt($("#toastArt"),p.banner,p.icon,p.torrent?"magnet":"file",p.artColor);
  $("#t").classList.toggle("has-art",p.kind==="finished");
  $("#toastWatermark").innerHTML="";
  applySurfaceArt(p.banner || p.icon || fallbackArt("file"),settings);
  $("#countdownTrack").hidden=!countdown; $("#timer").hidden=countdown;
  const actions=[];
  if(countdown) {
    renderCountdown(countdowns.get(p.token)||p);
    const label=({Exit:"Exit FluxDM now",Sleep:"Sleep now",Hibernate:"Hibernate now",Shutdown:"Shut down now",Restart:"Restart now"})[p.action]||"Run now";
    actions.push(button("Cancel","countdown_choice",{token:p.token,now:false},true),button(label,"countdown_choice",{token:p.token,now:true}));
  } else if(p.id && p.kind==="finished") {
    actions.push(button(p.torrent?"Open folder":"Open","open_item",{id:p.id},true),button("Show in folder","show_item",{id:p.id}));
  } else if(p.id && error) {
    actions.push(button("Retry now","start",{ids:[p.id]},true));
    if(p.kind==="retrying") actions.push(button("Pause","pause",{ids:[p.id]}));
  }
  $("#actions").replaceChildren(...actions);
  $("#t").title=reveal?"Click to show it in its folder":"Click to open FluxDM";
  updateTimer();
  invoke("toast_show",{gen:p.gen||0,height:Math.ceil(Math.max(110,$(".toast-content").getBoundingClientRect().height+2)),countdown}); // the content, not the card: the card stretches to the window
  refreshSharing();
}
function updateTimer() {
  if(!current) return;
  $("#timer").style.transform="scaleX("+Math.max(0,Math.min(1,(hideAt-Date.now())/SHOW_MS))+")";
}
function next() { if(queue.length) show(queue.shift()); }
function finish(clear=false) {
  const gen=current?.gen||0;
  current=null; hideAt=0;
  if(clear) queue.length=0;
  for(let i=queue.length-1;i>=0;i--) if(queue[i].kind==="countdown" && countdowns.get(queue[i].token)?.closed) queue.splice(i,1);
  if(queue.length) next(); else invoke("toast_hide",{gen});
}
function dismiss() { finish(true); }
setInterval(()=>{
  if(!current || current.kind==="countdown") return;
  // Hovering freezes the bar exactly where it is (it never jumps or restarts); after MAX_HOVER_MS it runs on.
  if(paused && Date.now()-shownAt<MAX_HOVER_MS) { hideAt=Date.now()+frozenLeft; return; }
  updateTimer();
  if(Date.now()>hideAt) finish();
},100);
$("#t").addEventListener("mouseenter",()=>{if(!paused) frozenLeft=Math.max(0,hideAt-Date.now()); paused=true;});
$("#t").addEventListener("mouseleave",()=>{if(paused) hideAt=Date.now()+Math.max(frozenLeft,1200); paused=false;});
$("#t").addEventListener("click",e=>{if(e.target.closest("button")) return;const r=reveal;dismiss();invoke("toast_click",{reveal:r});});
$("#x").addEventListener("click",()=>{if(current?.kind==="countdown") invoke("countdown_choice",{token:current.token,now:false});dismiss();});

async function refreshSharing() {
  const p=current;
  if(!p?.id || !p.torrent || p.kind!=="finished") return;
  try {
    const it=await invoke("toast_item",{id:p.id});
    if(current!==p || !it) return;
    if(it.seeding) {
      $("#body").replaceChildren("Now sharing at ",el("span",{class:"mono up",text:"\u2191 "+it.upSpeed}),...(it.ratio>0?[" until ratio ",el("span",{class:"mono",text:it.ratio})]:[]));
      $("#actions").replaceChildren(button("Open folder","open_item",{id:p.id},true),button("Stop sharing","pause",{ids:[p.id]}));
    }
  } catch {}
}
setInterval(refreshSharing,1000);

listen("appearance-changed",st=>{ settings=st; applySurfaceArt(current?.banner,st); });
