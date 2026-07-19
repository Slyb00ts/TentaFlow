// ===== File: DirectoryDecodeTests.cs — Directory CBOR decode parity tests =====
// Mirrors the wire shapes of tentaflow-sdk-spec::directory (DirectoryUsersOutput,
// DirectoryGroupsOutput, DirectoryRolesOutput, DirectoryOrgOutput). The email
// field is a CBOR Option: minicbor omits `None` map entries and a re-encoder
// may emit an explicit null — the decoder must accept both.

using TentaFlow.Sdk;
using TentaFlow.Sdk.Cbor;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class DirectoryDecodeTests
{
    [Fact]
    public void Users_FullRow_Decodes()
    {
        // {0: [{0: "u-1", 1: "jan", 2: "Jan Kowalski", 3: "jan@x", 4: ["g-1","g-2"], 5: true}]}
        var w = new CborWriter(128);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteArrayHeader(1);
        w.WriteMapHeader(6);
        w.WriteUInt(0); w.WriteText("u-1");
        w.WriteUInt(1); w.WriteText("jan");
        w.WriteUInt(2); w.WriteText("Jan Kowalski");
        w.WriteUInt(3); w.WriteText("jan@x");
        w.WriteUInt(4);
        w.WriteArrayHeader(2);
        w.WriteText("g-1");
        w.WriteText("g-2");
        w.WriteUInt(5); w.WriteBool(true);

        var users = Directory.DecodeUsers(w.ToArray());
        Assert.Single(users);
        Assert.Equal("u-1", users[0].Id);
        Assert.Equal("jan", users[0].Username);
        Assert.Equal("Jan Kowalski", users[0].DisplayName);
        Assert.Equal("jan@x", users[0].Email);
        Assert.Equal(new[] { "g-1", "g-2" }, users[0].Groups);
        Assert.True(users[0].IsActive);
    }

    [Fact]
    public void Users_NullEmail_DecodesAsNull()
    {
        // {0: [{0: "u-1", 1: "jan", 2: "", 3: null, 4: [], 5: true}]}
        var w = new CborWriter(64);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteArrayHeader(1);
        w.WriteMapHeader(6);
        w.WriteUInt(0); w.WriteText("u-1");
        w.WriteUInt(1); w.WriteText("jan");
        w.WriteUInt(2); w.WriteText("");
        w.WriteUInt(3); w.WriteNull();
        w.WriteUInt(4); w.WriteArrayHeader(0);
        w.WriteUInt(5); w.WriteBool(true);

        var users = Directory.DecodeUsers(w.ToArray());
        Assert.Single(users);
        Assert.Null(users[0].Email);
        Assert.Empty(users[0].Groups);
    }

    [Fact]
    public void Users_EmptyOutput_DecodesEmptyList()
    {
        var w = new CborWriter(8);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteArrayHeader(0);
        Assert.Empty(Directory.DecodeUsers(w.ToArray()));
    }

    [Fact]
    public void Groups_Decode()
    {
        // {0: [{0: "g-1", 1: "developers", 2: "Dev team", 3: 7}]}
        var w = new CborWriter(64);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteArrayHeader(1);
        w.WriteMapHeader(4);
        w.WriteUInt(0); w.WriteText("g-1");
        w.WriteUInt(1); w.WriteText("developers");
        w.WriteUInt(2); w.WriteText("Dev team");
        w.WriteUInt(3); w.WriteUInt(7);

        var groups = Directory.DecodeGroups(w.ToArray());
        Assert.Single(groups);
        Assert.Equal("g-1", groups[0].Id);
        Assert.Equal("developers", groups[0].Name);
        Assert.Equal("Dev team", groups[0].Description);
        Assert.Equal(7UL, groups[0].MemberCount);
    }

    [Fact]
    public void Roles_Decode()
    {
        // {0: [{0: "role-org-admin", 1: "org_admin"}]}
        var w = new CborWriter(64);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteArrayHeader(1);
        w.WriteMapHeader(2);
        w.WriteUInt(0); w.WriteText("role-org-admin");
        w.WriteUInt(1); w.WriteText("org_admin");

        var roles = Directory.DecodeRoles(w.ToArray());
        Assert.Single(roles);
        Assert.Equal("role-org-admin", roles[0].RoleId);
        Assert.Equal("org_admin", roles[0].Name);
    }

    [Fact]
    public void Org_Decode()
    {
        // {0: "org-default", 1: "Default Organization", 2: "default"}
        var w = new CborWriter(64);
        w.WriteMapHeader(3);
        w.WriteUInt(0); w.WriteText("org-default");
        w.WriteUInt(1); w.WriteText("Default Organization");
        w.WriteUInt(2); w.WriteText("default");

        var org = Directory.DecodeOrg(w.ToArray());
        Assert.Equal("org-default", org.OrgId);
        Assert.Equal("Default Organization", org.Name);
        Assert.Equal("default", org.Slug);
    }
}
