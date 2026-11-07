// Serveur local des maquettes (dev uniquement).
const h=require('http'),f=require('fs'),p=require('path');
const T={'.html':'text/html; charset=utf-8','.woff2':'font/woff2','.woff':'font/woff','.css':'text/css','.svg':'image/svg+xml','.png':'image/png'};
h.createServer((q,r)=>{const u=p.join(__dirname,decodeURIComponent(q.url.split('?')[0]));
 if(!u.startsWith(__dirname)){r.writeHead(403);return r.end()}
 f.readFile(u,(e,d)=>{if(e){r.writeHead(404);return r.end()}r.writeHead(200,{'content-type':T[p.extname(u)]||'application/octet-stream'});r.end(d)})}).listen(5178,'127.0.0.1');
