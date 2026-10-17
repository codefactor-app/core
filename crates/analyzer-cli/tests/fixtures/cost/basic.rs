fn constant()->usize{1}
fn fixed(){for i in 0..10{}}
fn linear(n:usize){for i in 0..n{}}
fn sequential(n:usize,m:usize){for i in 0..n{} for j in 0..m{}}
fn nested(n:usize,m:usize){for i in 0..n{for j in 0..m{}}}
fn triangular(n:usize){for i in 0..n{for j in 0..i{}}}
fn quadratic(n:usize){for i in 0..n{linear(n);}}
fn traversal(items:&[usize]){for item in items.iter(){}}
fn logarithmic(n:usize){let mut i=n;while i>0{i/=2;}}
fn unresolved(n:usize){foreign(n);}
fn recursive(n:usize){recursive(n);}
fn changed(mut n:usize){let mut i=0;while i<n{n=0;i+=1;}}
fn conditional_step(n:usize){let mut i=0;while i<n{if i>1{i+=1;}}}
fn zero(){for i in 0..0{foreign();}}
