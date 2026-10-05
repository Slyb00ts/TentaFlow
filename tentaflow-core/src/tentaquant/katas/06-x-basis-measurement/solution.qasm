OPENQASM 3.0;
include "stdgates.inc";

qubit[1] q;
bit[1] c;

x q[0];
h q[0];

h q[0];

c = measure q;
