// ===== File: AliasDecodeTests.cs — alias_list_available JSON decoding =====
// The host emits alias_list_available_v1 as a JSON `{ "aliases": [...] }`
// document (serde), so the C# wrapper parses JSON (not CBOR). These tests pin
// the field mapping to the host `AvailableAliasOut` shape in
// tentaflow-core/src/addon/host_functions/aliases.rs.

#nullable enable

using System.Text;
using TentaFlow.Sdk;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class AliasDecodeTests
{
    private static byte[] Utf8(string s) => Encoding.UTF8.GetBytes(s);

    [Fact]
    public void ParsesAllFields()
    {
        var json = Utf8("""
        {"aliases":[
          {"alias_id":"translator-llm","target_model":"Bielik-Minitron-7B-v3.0-Instruct-FP8",
           "methods":["chat","generate"],"strategy":"priority","grant_status":"granted",
           "visibility":"restricted","active":true,"required":false}
        ]}
        """);

        var list = Aliases.ParseAvailable(json);

        Assert.Single(list);
        var a = list[0];
        Assert.Equal("translator-llm", a.AliasId);
        Assert.Equal("Bielik-Minitron-7B-v3.0-Instruct-FP8", a.TargetModel);
        Assert.Equal(new[] { "chat", "generate" }, a.Methods);
        Assert.Equal("priority", a.Strategy);
        Assert.Equal("granted", a.GrantStatus);
        Assert.Equal("restricted", a.Visibility);
        Assert.True(a.Active);
        Assert.False(a.Required);
    }

    [Fact]
    public void HandlesNullOptionalsAndPendingGrant()
    {
        // A pending alias (owner not installed yet): target_model/strategy/visibility null.
        var json = Utf8("""
        {"aliases":[
          {"alias_id":"translator-stt","target_model":null,"methods":[],
           "strategy":null,"grant_status":"pending","visibility":null,
           "active":false,"required":true}
        ]}
        """);

        var a = Assert.Single(Aliases.ParseAvailable(json));
        Assert.Equal("translator-stt", a.AliasId);
        Assert.Null(a.TargetModel);
        Assert.Empty(a.Methods);
        Assert.Null(a.Strategy);
        Assert.Equal("pending", a.GrantStatus);
        Assert.Null(a.Visibility);
        Assert.False(a.Active);
        Assert.True(a.Required);
    }

    [Fact]
    public void EmptyListYieldsNoEntries()
    {
        Assert.Empty(Aliases.ParseAvailable(Utf8("""{"aliases":[]}""")));
    }
}
