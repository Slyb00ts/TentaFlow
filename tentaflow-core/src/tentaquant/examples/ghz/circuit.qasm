OPENQASM 3.0;
include "stdgates.inc";

qubit[5] q;
bit[5] c;

h q[0];
for int i in [1:4] {
    cx q[0], q[i];
}

c = measure q;
