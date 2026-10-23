# Uses of a slot-held environment, per opcode (see env_census.sh).
/^function / { delete held; next }
{ op = $1; gsub(/,/, "") }
op ~ /^LoadFromEnvironment/ {
  if ($3 in held) n["load through a slot-held env"]++
  held[$2] = 1
  next
}
op ~ /^Store(NP)?ToEnvironment/ { if ($2 in held) n["store through a slot-held env"]++; next }
op == "GetEnvironment" && NF >= 4 { if ($3 in held) n["GetEnvironment from a slot-held env"]++; delete held[$2]; next }
op ~ /^Create(Closure|Generator|AsyncClosure)/ { if ($3 in held) n["closure created with a slot-held env"]++; delete held[$2]; next }
op == "CreateEnvironment" && NF >= 4 { if ($3 in held) n["CreateEnvironment on a slot-held parent"]++; delete held[$2]; next }
NF >= 2 && $2 ~ /^r[0-9]+$/ { delete held[$2] }
END {
  print "slot-held environments:"
  for (k in n) printf "  %7d  %s\n", n[k], k
}
