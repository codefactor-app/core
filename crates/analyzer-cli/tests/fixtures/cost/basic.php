<?php
function constant(){return 1;}
function fixed(){for($i=0;$i<10;$i++){}}
function linear(int $n){for($i=0;$i<$n;$i++){}}
function sequential(int $n,int $m){for($i=0;$i<$n;$i++){} for($j=0;$j<$m;$j++){}}
function nested(int $n,int $m){for($i=0;$i<$n;$i++){for($j=0;$j<$m;$j++){}}}
function triangular(int $n){for($i=0;$i<$n;$i++){for($j=0;$j<$i;$j++){}}}
function quadratic(int $n){for($i=0;$i<$n;$i++){linear($n);}}
function traversal(array $items){foreach($items as $item){}}
function logarithmic(int $n){$i=$n;while($i>0){$i=intdiv($i,2);}}
function unresolved(int $n){foreign($n);}
function recursive(int $n){recursive($n);}
function changed(int $n){for($i=0;$i<$n;$i++){$n=0;}}
function conditional_step(int $n){$i=0;while($i<$n){if($i>1){$i++;}}}
function zero(){for($i=0;$i<0;$i++){foreign();}}
