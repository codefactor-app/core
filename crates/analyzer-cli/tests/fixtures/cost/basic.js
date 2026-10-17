function constant(){return 1;}
function fixed(){for(let i=0;i<10;i++){}}
function linear(n){for(let i=0;i<n;i++){}}
function sequential(n,m){for(let i=0;i<n;i++){} for(let j=0;j<m;j++){}}
function nested(n,m){for(let i=0;i<n;i++){for(let j=0;j<m;j++){}}}
function triangular(n){for(let i=0;i<n;i++){for(let j=0;j<i;j++){}}}
function quadratic(n){for(let i=0;i<n;i++){linear(n);}}
function traversal(items){for(const item of items){}}
function logarithmic(n){let i=n;while(i>0){i=Math.floor(i/2);}}
function unresolved(n){foreign(n);}
function recursive(n){recursive(n);}
function changed(n){for(let i=0;i<n;i++){n=0;}}
function conditional_step(n){let i=0;while(i<n){if(i>1){i++;}}}
function zero(){for(let i=0;i<0;i++){foreign();}}
