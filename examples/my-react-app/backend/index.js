const readline=require('node:readline');
const input=readline.createInterface({input:process.stdin});
input.on('line',line=>{const r=JSON.parse(line);if(r.type==='shutdown'){input.close();return;}process.stdout.write(JSON.stringify({protocol:1,id:r.id,result:{ok:true}})+'\n')});
