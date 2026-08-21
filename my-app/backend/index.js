const readline=require('node:readline');
readline.createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);process.stdout.write(JSON.stringify({protocol:1,id:r.id,result:{ok:true}})+'\n')});
